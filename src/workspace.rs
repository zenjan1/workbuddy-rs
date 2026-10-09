use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::plan::Plan;

/// 任务工作区:每次 run 在 workspace_dir 下生成独立目录,
/// 持久化计划、各步骤产出与执行结果,供 list/show 查询。
pub struct Workspace {
    pub dir: PathBuf,
}

#[derive(Serialize, Deserialize)]
pub struct StepRecord {
    pub id: usize,
    pub name: String,
    pub agent: String,
    pub skill: String,
    pub kind: String,
    pub ok: bool,
    pub error: Option<String>,
    pub seconds: f64,
    pub output_preview: String,
}

#[derive(Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    pub goal: String,
    pub created: String,
    pub model: String,
    pub mock: bool,
    pub steps: Vec<StepRecord>,
}

impl Workspace {
    /// 创建新的任务工作区目录(含 outputs 子目录)。
    pub fn create_new(root: &Path) -> Result<Self> {
        let id = new_run_id();
        let dir = root.join(&id);
        std::fs::create_dir_all(dir.join("outputs"))
            .with_context(|| format!("创建工作区失败: {}", dir.display()))?;
        Ok(Self { dir })
    }

    /// 打开已存在的任务工作区。
    pub fn open(root: &Path, id: &str) -> Result<Self> {
        let dir = root.join(id);
        if !dir.is_dir() {
            anyhow::bail!("找不到任务 {}: {}", id, dir.display());
        }
        Ok(Self { dir })
    }

    pub fn root(root: &Path) -> Vec<RunRecord> {
        let Ok(entries) = std::fs::read_dir(root) else {
            return Vec::new();
        };
        let mut runs = Vec::new();
        for e in entries.flatten() {
            if let Ok(rec) = Self::open(root, &e.file_name().to_string_lossy())
                .and_then(|ws| ws.load_record())
            {
                runs.push(rec);
            }
        }
        runs.sort_by(|a, b| b.id.cmp(&a.id));
        runs
    }

    pub fn load_record(&self) -> Result<RunRecord> {
        let meta = serde_json::from_str(&std::fs::read_to_string(self.meta_path())?)?;
        Ok(meta)
    }

    pub fn meta_path(&self) -> PathBuf {
        self.dir.join("meta.json")
    }

    /// 规划阶段:落盘 goal 与 plan。
    pub fn save_plan(&self, run: &RunRecord, plan: &Plan) -> Result<()> {
        std::fs::write(
            self.dir.join("plan.json"),
            serde_json::to_string_pretty(plan)?,
        )
        .context("写入 plan.json 失败")?;
        std::fs::write(
            self.meta_path(),
            serde_json::to_string_pretty(run)?,
        )
        .context("写入 meta.json 失败")?;
        Ok(())
    }

    /// 执行阶段:保存某步骤的完整产出。
    pub fn save_step_output(&self, id: usize, text: &str) -> Result<()> {
        std::fs::write(self.dir.join("outputs").join(format!("{id}.md")), text)
            .with_context(|| format!("写入步骤 {} 产出失败", id))
    }

    /// 执行阶段:更新 meta 中的步骤结果与最终报告。
    pub fn finish_run(&self, run: &mut RunRecord, report: &str) -> Result<()> {
        std::fs::write(self.dir.join("report.md"), report)
            .context("写入 report.md 失败")?;
        std::fs::write(
            self.meta_path(),
            serde_json::to_string_pretty(run)?,
        )
        .context("更新 meta.json 失败")?;
        Ok(())
    }
}

static RUN_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// 生成 run id:unix 秒十六进制-进程内序号哈希(同秒内保证不重复)。
fn new_run_id() -> String {
    use std::sync::atomic::Ordering;
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let seq = RUN_SEQ.fetch_add(1, Ordering::Relaxed);
    let mut h = 0u64;
    for _ in 0..32 {
        h = h
            .wrapping_mul(6364136223846793005)
            .wrapping_add(t.wrapping_add(1442695040888963407).wrapping_add(seq as u64));
    }
    // 前缀 13 位(世纪内足够)+ 后缀固定 8 位十六进制,总长恒定 22
    format!("{:013x}-{:08x}", t, h as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn run_id_is_unique_and_formatted() {
        let a = new_run_id();
        let b = new_run_id();
        assert_eq!(a.len(), b.len());
        assert!(a.contains('-'));
        // 同一时刻可能前缀相同,后缀应不同(或整体不同)
        assert_ne!(a, b);
    }

    #[test]
    fn workspace_lifecycle() {
        let tmp = std::env::temp_dir().join(format!("wb-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cfg = Config {
            api_key: None,
            base_url: "x".into(),
            model: "x".into(),
            max_parallel: 1,
            workspace_dir: tmp.clone(),
            repo_dir: tmp.clone(),
            request_timeout_secs: 1,
            mock: true,
        };
        let ws = Workspace::create_new(&cfg.workspace_dir).unwrap();
        let mut run = RunRecord {
            id: ws.dir.file_name().unwrap().to_string_lossy().to_string(),
            goal: "测试".into(),
            created: "now".into(),
            model: "mock".into(),
            mock: true,
            steps: vec![],
        };
        ws.save_step_output(1, "产出内容").unwrap();
        let plan = crate::plan::Plan::from_llm(
            "测试",
            r#"{"goal":"测试","steps":[{"id":1,"name":"a","agent":"writer","skill":"report-writing","prompt":"p","kind":"llm","needs":[]}]}"#,
        )
        .unwrap();
        ws.save_plan(&run, &plan).unwrap();
        run.steps.push(StepRecord {
            id: 1,
            name: "a".into(),
            agent: "writer".into(),
            skill: "report-writing".into(),
            kind: "llm".into(),
            ok: true,
            error: None,
            seconds: 0.1,
            output_preview: "产出内容".into(),
        });
        ws.finish_run(&mut run, "# 报告\n完成").unwrap();

        // 重新打开并校验
        let ws2 = Workspace::open(&cfg.workspace_dir, &run.id).unwrap();
        let rec = ws2.load_record().unwrap();
        assert_eq!(rec.steps.len(), 1);
        let report = std::fs::read_to_string(ws2.dir.join("report.md")).unwrap();
        assert!(report.contains("报告"));
        let listed = Workspace::root(&cfg.workspace_dir);
        assert_eq!(listed.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
