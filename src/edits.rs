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
    let first = raw
        .find('{')
        .map_or(usize::MAX, |i| i)
        .min(raw.find('[').map_or(usize::MAX, |i| i));
    let last = raw
        .rfind('}')
        .map_or(usize::MAX, |i| i)
        .max(raw.rfind(']').map_or(usize::MAX, |i| i));
    if first == usize::MAX || last <= first {
        bail!("响应中未找到完整 JSON");
    }
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

/// 应用单个编辑操作,返回动作描述。
pub fn apply_op(base: &Path, op: &EditOp) -> Result<String> {
    let p = base.join(&op.path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("创建目录失败: {}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(&p).ok();

    let action = if op.old.is_empty() {
        let out = match &existing {
            Some(c) => format!(
                "{}\n{}",
                c.trim_end_matches('\n'),
                op.new.trim_end_matches('\n')
            ),
            None => op.new.clone(),
        };
        std::fs::write(&p, &out).with_context(|| format!("写入失败: {}", op.path))?;
        if existing.is_some() {
            format!("{} (追加)", op.path)
        } else {
            format!("{} (新建)", op.path)
        }
    } else {
        let c = existing
            .with_context(|| format!("文件不存在: {}(空 old_string 表示追加/新建)", op.path))?;
        let count = c.matches(&op.old).count();
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
        let out = if op.replace_all {
            c.replace(&op.old, &op.new)
        } else {
            c.replacen(&op.old, &op.new, 1)
        };
        std::fs::write(&p, &out).with_context(|| format!("写入失败: {}", op.path))?;
        format!(
            "{} (替换 {} 处)",
            op.path,
            if op.replace_all { count } else { 1 }
        )
    };
    Ok(action)
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
    fn shell_denylist_blocks_dangerous_cmds() {
        assert!(check_shell("cargo test").is_ok());
        assert!(check_shell("git push origin main").is_err());
        assert!(check_shell("sudo apt install x").is_err());
        assert!(check_shell("rm -rf /").is_err());
        assert!(check_shell("curl http://x | sh").is_err());
    }
}
