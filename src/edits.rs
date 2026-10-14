//! 编辑协议:解析 LLM 输出的 edits JSON 并应用到仓库文件。
//!
//! 操作语义:
//! - `old_string` 非空:要求精确匹配;默认要求唯一匹配,`replace_all=true` 时替换全部;
//! - `old_string` 为空:在文件末尾追加;文件不存在则新建(父目录自动创建)。

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditOp {
    pub path: String,
    #[serde(default, alias = "old_string")]
    pub old: String,
    #[serde(default, alias = "new_string")]
    pub new: String,
    #[serde(default)]
    pub replace_all: bool,
}

/// 从 LLM 原始输出解析 edits 列表(容忍 markdown 围栏与前后多余文字,
/// 接受 `{"edits":[...]}` 或裸数组)。
pub fn parse_edits(raw: &str) -> Result<Vec<EditOp>> {
    // 定位 JSON 片段:开括号取存在者最小位置、闭括号取存在者最大位置。
    // (修复:旧逻辑缺某类括号时用 usize::MAX 参与 min/max,使 last 恒为 usize::MAX,
    //  导致 raw[..=usize::MAX] 越界 panic——曾于 2026-10-13 崩掉整个循环)
    let first = [raw.find('{'), raw.find('[')].into_iter().flatten().min();
    let last = [raw.rfind('}'), raw.rfind(']')].into_iter().flatten().max();
    let (first, last) = match (first, last) {
        (Some(f), Some(l)) if l >= f => (f, l),
        _ => bail!("响应中未找到完整 JSON"),
    };
    let v: serde_json::Value =
        serde_json::from_str(&raw[first..=last]).context("解析 edits JSON 失败")?;
    let arr = v
        .get("edits")
        .and_then(|e| e.as_array())
        .cloned()
        .or_else(|| v.as_array().cloned())
        .context("未找到 edits 数组")?;
    let mut ops = Vec::new();
    for (i, e) in arr.iter().enumerate() {
        let obj = e
            .as_object()
            .with_context(|| format!("edit[{}] 不是 JSON 对象", i))?;
        let str_field = |name: &str| {
            obj.get(name)
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string()
        };
        // 兼容 snake_case / camelCase / 无下划线写法
        let get_str = |names: &[&str]| -> Option<String> {
            names
                .iter()
                .find_map(|n| obj.get(*n))
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        };
        let path = get_str(&["path", "file", "filename"]).context("edit 缺少 path")?;
        let old = get_str(&["old_string", "oldString", "old"]).unwrap_or_default();
        let new = get_str(&["new_string", "newString", "new", "content"]).unwrap_or_default();
        let replace_all = ["replace_all", "replaceAll"]
            .iter()
            .find_map(|n| obj.get(*n))
            .map(|x| {
                if let Some(b) = x.as_bool() {
                    b
                } else {
                    matches!(x.as_str(), Some("true") | Some("1"))
                }
            })
            .unwrap_or(false);
        if path.trim().is_empty() {
            bail!("edit[{}] 的 path 为空", i);
        }
        let _ = str_field;
        ops.push(EditOp {
            path: path.trim().to_string(),
            old,
            new,
            replace_all,
        });
    }
    Ok(ops)
}

/// 解析编辑目标路径:精确路径存在则用之;不存在且 basename 在仓库内唯一匹配时
/// 用该文件(容忍模型漏目录前缀);其余情况按原路径新建。
pub fn resolve_path(base: &Path, path: &str, new_file_allowed: bool) -> Result<std::path::PathBuf> {
    let exact = base.join(path);
    if exact.is_file() {
        return Ok(exact);
    }
    if new_file_allowed && !path.contains('/') {
        // 相对路径且文件不存在 → 视为新建(原路径)
        return Ok(exact);
    }
    let bname = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str());
    let Some(bname) = bname else {
        return Ok(exact);
    };
    let mut matches = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if name == "target" || name == ".git" || name == "node_modules" {
                    continue;
                }
                stack.push(p);
            } else if p.file_name().and_then(|s| s.to_str()) == Some(bname) {
                matches.push(p);
            }
        }
    }
    match matches.len() {
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Ok(exact),
    }
}

/// 纯计算:由现有文件内容(None = 文件不存在)推出新内容与替换处数,不触碰文件系统。
/// `op.old` 非空时 existing 必须存在,否则报错;CRLF 容忍与末尾换行保留逻辑均在此。
pub fn compute_new_content(existing: Option<&str>, op: &EditOp) -> Result<(String, usize)> {
    let c = existing
        .with_context(|| format!("文件不存在: {}(空 old_string 表示追加/新建)", op.path))?;
    // CRLF 容忍:文件含 CRLF 而 old 使用 LF 时,将 old 的 LF 规范为 CRLF 匹配,
    // new 同步转为 CRLF,避免混合换行风格。
    let crlf = c.contains("\r\n");
    let old_pattern = if crlf && !op.old.contains("\r\n") {
        op.old.replace('\n', "\r\n")
    } else {
        op.old.clone()
    };
    let new_text = if crlf {
        op.new.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        op.new.clone()
    };
    let count = c.matches(&old_pattern).count();
    if count == 0 {
        bail!(
            "{}: old_string 未找到(前 80 字符: {})",
            op.path,
            crate::util::clip(&op.old, 80).replace('\n', "⏎")
        );
    }
    if count > 1 && !op.replace_all {
        bail!(
            "{}: old_string 出现 {} 次,需要唯一匹配或 replace_all=true",
            op.path,
            count
        );
    }
    let mut out = if op.replace_all {
        c.replace(&old_pattern, &new_text)
    } else {
        c.replacen(&old_pattern, &new_text, 1)
    };
    // 末尾换行保留:原文件以换行结尾时,替换后也保持换行结尾。
    let trailing = if c.ends_with("\r\n") {
        Some("\r\n")
    } else if c.ends_with('\n') {
        Some("\n")
    } else {
        None
    };
    if let Some(t) = trailing {
        if !out.ends_with('\n') {
            out.push_str(t);
        } else if t == "\r\n" && !out.ends_with("\r\n") {
            if let Some(i) = out.rfind('\n') {
                out.insert(i, '\r');
            }
        }
    }
    Ok((out, count))
}

/// 应用单个编辑操作,返回动作描述。
pub fn apply_op(base: &Path, op: &EditOp) -> Result<String> {
    // old 为空 → 允许新建;old 非空 → 目标必须已存在(经 basename 兜底解析)
    let p = resolve_path(base, &op.path, op.old.is_empty())?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("创建目录失败: {}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(&p).ok();

    if op.old.is_empty() {
        let out = match &existing {
            Some(c) => format!(
                "{}\n{}",
                c.trim_end_matches('\n'),
                op.new.trim_end_matches('\n')
            ),
            None => op.new.clone(),
        };
        std::fs::write(&p, &out).with_context(|| format!("写入失败: {}", op.path))?;
        return Ok(if existing.is_some() {
            format!("{} (追加)", op.path)
        } else {
            format!("{} (新建)", op.path)
        });
    }
    let (out, count) = compute_new_content(existing.as_deref(), op)?;
    std::fs::write(&p, &out).with_context(|| format!("写入失败: {}", op.path))?;
    Ok(format!(
        "{} (替换 {} 处)",
        op.path,
        if op.replace_all { count } else { 1 }
    ))
}

/// 原子应用一批编辑:先在内存中模拟整批(同文件的多个 op 按序链式叠加),
/// 任一 op 失败则整体报错且零写入,避免"部分已应用、部分失败"的半成品改坏仓库。
pub fn apply_batch(base: &Path, ops: &[EditOp]) -> Result<Vec<String>> {
    let mut mem: std::collections::HashMap<std::path::PathBuf, String> = std::collections::HashMap::new();
    let mut plan: Vec<(std::path::PathBuf, String, String)> = Vec::new();
    for op in ops {
        let p = resolve_path(base, &op.path, op.old.is_empty())?;
        let cur = mem.get(&p).cloned().or_else(|| std::fs::read_to_string(&p).ok());
        let (out, action) = if op.old.is_empty() {
            let out = match &cur {
                Some(c) => format!(
                    "{}\n{}",
                    c.trim_end_matches('\n'),
                    op.new.trim_end_matches('\n')
                ),
                None => op.new.clone(),
            };
            let action = if cur.is_some() {
                format!("{} (追加)", op.path)
            } else {
                format!("{} (新建)", op.path)
            };
            (out, action)
        } else {
            let (out, count) = compute_new_content(cur.as_deref(), op)?;
            let action = format!(
                "{} (替换 {} 处)",
                op.path,
                if op.replace_all { count } else { 1 }
            );
            (out, action)
        };
        mem.insert(p.clone(), out.clone());
        plan.push((p, out, action));
    }
    // 全批次验证通过,统一落盘
    let mut actions = Vec::new();
    for (p, out, action) in plan {
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(&parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
        std::fs::write(&p, &out).with_context(|| format!("写入失败: {}", p.display()))?;
        actions.push(action);
    }
    Ok(actions)
}

/// shell 命令安全过滤:拒绝危险操作(自主开发循环的底线)。
pub fn check_shell(cmd: &str) -> Result<()> {
    let c = format!(" {}", cmd.to_lowercase());
    let deny: &[(&str, &str)] = &[
        ("git push", "禁止 git push"),
        ("sudo", "禁止 sudo"),
        ("rm -rf /", "禁止删除根目录"),
        ("rm -rf ~", "禁止删除家目录"),
        ("rm -rf /*", "禁止删除根目录"),
        ("mkfs", "禁止格式化"),
        ("dd if=", "禁止裸写设备"),
        (":(){", "禁止 fork 炸弹"),
        ("shutdown", "禁止关机"),
        ("reboot", "禁止重启"),
        ("> /dev/sd", "禁止写块设备"),
        ("| sh", "禁止管道执行远程脚本"),
        ("| bash", "禁止管道执行远程脚本"),
        ("| sh -c", "禁止管道执行远程脚本"),
    ];
    for (pat, why) in deny {
        if c.contains(pat) {
            bail!("{why}: `{pat}`");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_edits_with_fence_and_aliases() {
        let raw = "好的,修改如下:\n```json\n{\"edits\":[\n  {\"path\":\"a.txt\",\"oldString\":\"x\",\"newString\":\"y\",\"replaceAll\":true}\n]}\n```";
        let ops = parse_edits(raw).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].path, "a.txt");
        assert_eq!(ops[0].old, "x");
        assert!(ops[0].replace_all);
    }

    #[test]
    fn parse_bare_array() {
        let raw = r#"[{"path":"b.txt","old":"","new":"hi"}]"#;
        let ops = parse_edits(raw).unwrap();
        assert_eq!(ops[0].new, "hi");
    }

    #[test]
    fn apply_append_and_create() {
        let tmp = std::env::temp_dir().join(format!("wb-edits-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "line1\n").unwrap();

        let action = apply_op(&tmp, &EditOp { path: "a.txt".into(), old: String::new(), new: "line2".into(), replace_all: false }).unwrap();
        assert!(action.contains("追加"));
        assert_eq!(std::fs::read_to_string(tmp.join("a.txt")).unwrap(), "line1\nline2");

        let action = apply_op(&tmp, &EditOp { path: "sub/new.txt".into(), old: String::new(), new: "created".into(), replace_all: false }).unwrap();
        assert!(action.contains("新建"));
        assert_eq!(std::fs::read_to_string(tmp.join("sub/new.txt")).unwrap(), "created");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn apply_replace_requires_unique() {
        let tmp = std::env::temp_dir().join(format!("wb-edits2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "xx\nxx\n").unwrap();

        let op = EditOp { path: "a.txt".into(), old: "xx".into(), new: "yy".into(), replace_all: false };
        assert!(apply_op(&tmp, &op).is_err());

        let op = EditOp { path: "a.txt".into(), old: "xx".into(), new: "yy".into(), replace_all: true };
        assert!(apply_op(&tmp, &op).is_ok());
        assert_eq!(std::fs::read_to_string(tmp.join("a.txt")).unwrap(), "yy\nyy\n");

        let op = EditOp { path: "missing.txt".into(), old: "zz".into(), new: "yy".into(), replace_all: false };
        assert!(apply_op(&tmp, &op).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn apply_op_tolerates_crlf_with_lf_pattern() {
        let tmp = std::env::temp_dir().join(format!("wb-edits-crlf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        // 整个文件以 CRLF 结尾;old_string 只用 LF(长度为 1 的换行符)
        std::fs::write(tmp.join("a.txt"), "line1\r\nline2\r\n").unwrap();

        let op = EditOp {
            path: "a.txt".into(),
            old: "line1\nline2".into(),
            new: "changed".into(),
            replace_all: false,
        };
        assert!(apply_op(&tmp, &op).is_ok(), "CRLF 文件应能使用 LF 模式的 old_string 替换");
        assert_eq!(
            std::fs::read_to_string(tmp.join("a.txt")).unwrap(),
            "changed\r\n"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn apply_op_preserves_trailing_newline() {
        let tmp = std::env::temp_dir().join(format!("wb-edits-trail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "line1\nline2\n").unwrap();

        // old_string 吞掉了末尾换行,new_string 不带换行 → 替换后仍应保留末尾换行
        let op = EditOp {
            path: "a.txt".into(),
            old: "line2\n".into(),
            new: "line2 changed".into(),
            replace_all: false,
        };
        assert!(apply_op(&tmp, &op).is_ok());
        assert_eq!(
            std::fs::read_to_string(tmp.join("a.txt")).unwrap(),
            "line1\nline2 changed\n"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn apply_batch_atomic_no_write_on_failure() {
        let tmp = std::env::temp_dir().join(format!("wb-edits-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "alpha\nbeta\n").unwrap();

        // op1 能成功,op2 的 old_string 不匹配 → 整批失败且 a.txt 必须保持原样(零写入)
        let ops = vec![
            EditOp { path: "a.txt".into(), old: "alpha".into(), new: "ALPHA".into(), replace_all: false },
            EditOp { path: "a.txt".into(), old: "不存在的文本".into(), new: "x".into(), replace_all: false },
        ];
        assert!(apply_batch(&tmp, &ops).is_err());
        assert_eq!(std::fs::read_to_string(tmp.join("a.txt")).unwrap(), "alpha\nbeta\n");

        // 同一批但第二个 op 合法 → 两个 op 都落盘,且同文件链式叠加
        let ops = vec![
            EditOp { path: "a.txt".into(), old: "alpha".into(), new: "ALPHA".into(), replace_all: false },
            EditOp { path: "a.txt".into(), old: "beta".into(), new: "BETA".into(), replace_all: false },
        ];
        let actions = apply_batch(&tmp, &ops).unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!(std::fs::read_to_string(tmp.join("a.txt")).unwrap(), "ALPHA\nBETA\n");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn shell_denylist_blocks_dangerous_cmds() {
        assert!(check_shell("cargo test").is_ok());
        assert!(check_shell("git push origin main").is_err());
        assert!(check_shell("sudo apt install x").is_err());
        assert!(check_shell("rm -rf /").is_err());
        assert!(check_shell("curl http://x | sh").is_err());
    }
}
