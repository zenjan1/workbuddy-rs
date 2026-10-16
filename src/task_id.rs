use std::collections::HashSet;
use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

/// TaskId 用于统一表示旧数字索引和基于标题的短 slug。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TaskId {
    /// 兼容旧版数字索引
    Numeric(usize),
    /// 标题生成的短 slug
    Slug(String),
}

impl TaskId {
    pub fn from_numeric(n: usize) -> Self {
        TaskId::Numeric(n)
    }

    pub fn from_slug(s: impl Into<String>) -> Self {
        TaskId::Slug(s.into())
    }

    /// 将标题转换为 slug 基础形式。
    /// 规则：小写、非字母数字转连字符、去除首尾连字符、连续连字符合并。
    /// 如果结果为空，返回空字符串，调用方应回退到数字索引。
    pub fn slugify(title: &str) -> String {
        let mut out = String::new();
        let mut last_dash = false;

        for ch in title.trim().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_lowercase());
                last_dash = false;
            } else if !out.is_empty() && !last_dash {
                out.push('-');
                last_dash = true;
            }
        }

        while out.ends_with('-') {
            out.pop();
        }
        out
    }

    /// 根据已有 slug 集合解决冲突：若 base 已存在，则追加 -2、-3 ...
    pub fn unique_slug(base: &str, existing_slugs: &HashSet<&str>) -> String {
        if !existing_slugs.contains(base) {
            return base.to_string();
        }

        let mut n = 2;
        loop {
            let candidate = format!("{}-{}", base, n);
            if !existing_slugs.contains(candidate.as_str()) {
                return candidate;
            }
            n += 1;
        }
    }

    /// 从标题生成 TaskId。
    /// 空标题或 slugify 后为空时回退到数字索引；否则生成唯一 slug。
    pub fn from_title(title: &str, numeric_fallback: usize, existing_slugs: &HashSet<&str>) -> Self {
        let base = Self::slugify(title);
        if base.is_empty() {
            return TaskId::Numeric(numeric_fallback);
        }
        TaskId::Slug(Self::unique_slug(&base, existing_slugs))
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskId::Numeric(n) => write!(f, "{}", n),
            TaskId::Slug(s) => write!(f, "{}", s),
        }
    }
}

impl FromStr for TaskId {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(n) = s.parse::<usize>() {
            Ok(TaskId::Numeric(n))
        } else {
            Ok(TaskId::Slug(s.to_string()))
        }
    }
}

/// 表示标题解析失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskIdError {
    /// 未找到匹配任务
    NotFound { query: String },
    /// 找到多个匹配任务，返回候选标题。
    Ambiguous { query: String, matches: Vec<String> },
    /// 找到多个匹配任务，但它们的标题完全相同。
    DuplicateTitle { query: String, title: String },
}

impl fmt::Display for TaskIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskIdError::NotFound { query } => write!(f, "task id '{}' not found", query),
            TaskIdError::Ambiguous { query, matches } => {
                write!(f, "task id '{}' is ambiguous; matches: {}", query, matches.join(", "))
            }
            TaskIdError::DuplicateTitle { query, title } => {
                write!(f, "task id '{}' is duplicated; title: {}", query, title)
            }
        }
    }
}

impl std::error::Error for TaskIdError {}

fn normalize_title(title: &str) -> String {
    title.trim().to_lowercase()
}

fn duplicate_or_ambiguous(query: &str, matches: Vec<(usize, String)>) -> TaskIdError {
    let unique_titles: HashSet<String> = matches
        .iter()
        .map(|(_, title)| normalize_title(title))
        .collect();
    if unique_titles.len() == 1 {
        TaskIdError::DuplicateTitle {
            query: query.to_string(),
            title: matches[0].1.clone(),
        }
    } else {
        TaskIdError::Ambiguous {
            query: query.to_string(),
            matches: matches.into_iter().map(|(_, title)| title).collect(),
        }
    }
}

/// 解析原始 task_id 参数。
///
/// * 若参数可解析为纯数字索引，则直接返回该数字，保持旧行为。
/// * 否则将参数和每个 backlog 标题做 slug 化后比较。
///   唯一匹配返回其在 `titles` 中的内部索引（从 0 开始）。
pub fn resolve_task_id<I, S>(raw: &str, titles: I) -> Result<usize, TaskIdError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if let Ok(index) = raw.parse::<usize>() {
        return Ok(index);
    }

    let title_list: Vec<String> = titles
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .collect();

    let normalized = TaskId::slugify(raw);
    if normalized.is_empty() {
        return Err(TaskIdError::NotFound {
            query: raw.to_string(),
        });
    }

    // 精确 slug 匹配：保持原有的 slugify 后比较逻辑。
    let mut matches = Vec::new();
    for (index, title) in title_list.iter().enumerate() {
        if TaskId::slugify(title) == normalized {
            matches.push((index, title.clone()));
        }
    }

    match matches.len() {
        0 => {
            // 精确 slug 匹配未命中，进行模糊匹配：标题归一化或 slug 包含查询。
            let query_normalized = normalize_title(raw);
            let query_slug = normalized.as_str();
            for (index, title) in title_list.iter().enumerate() {
                let title_normalized = normalize_title(title);
                let title_slug = TaskId::slugify(title);
                if title_normalized.contains(query_normalized.as_str()) || title_slug.contains(query_slug) {
                    matches.push((index, title.clone()));
                }
            }

            match matches.len() {
                0 => Err(TaskIdError::NotFound {
                    query: raw.to_string(),
                }),
                1 => Ok(matches[0].0),
                _ => Err(duplicate_or_ambiguous(raw, matches)),
            }
        }
        1 => Ok(matches[0].0),
        _ => Err(duplicate_or_ambiguous(raw, matches)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn slugify_lowercases_and_dashes() {
        assert_eq!(TaskId::slugify("  Hello World  "), "hello-world");
        assert_eq!(TaskId::slugify("Hello   World"), "hello-world");
        assert_eq!(TaskId::slugify("!!Foo__Bar--Baz"), "foo-bar-baz");
    }

    #[test]
    fn slugify_empty_returns_empty() {
        assert_eq!(TaskId::slugify(""), "");
        assert_eq!(TaskId::slugify("---"), "");
    }

    #[test]
    fn from_str_numeric_compat() {
        let id: TaskId = "123".parse().unwrap();
        assert_eq!(id, TaskId::Numeric(123));
        assert_eq!(id.to_string(), "123");
    }

    #[test]
    fn from_str_slug() {
        let id: TaskId = "foo-bar".parse().unwrap();
        assert_eq!(id, TaskId::Slug("foo-bar".to_string()));
        assert_eq!(id.to_string(), "foo-bar");
    }

    #[test]
    fn from_title_falls_back_to_numeric_when_empty() {
        let existing = HashSet::new();
        assert_eq!(TaskId::from_title("", 42, &existing), TaskId::Numeric(42));
        assert_eq!(TaskId::from_title("!!!", 7, &existing), TaskId::Numeric(7));
    }

    #[test]
    fn from_title_resolves_conflicts() {
        let mut existing = HashSet::new();
        existing.insert("hello");
        existing.insert("hello-2");
        let id = TaskId::from_title("Hello", 1, &existing);
        assert_eq!(id, TaskId::Slug("hello-3".to_string()));
    }

    #[test]
    fn unique_slug_appends_suffix() {
        let mut existing = HashSet::new();
        existing.insert("task");
        existing.insert("task-2");
        assert_eq!(TaskId::unique_slug("task", &existing), "task-3");
        assert_eq!(TaskId::unique_slug("brand-new", &existing), "brand-new");
    }

    #[test]
    fn resolve_numeric_index_passthrough() {
        let titles = ["Write docs", "Fix bug"];
        assert_eq!(resolve_task_id("1", titles), Ok(1));
        assert_eq!(resolve_task_id("0", titles), Ok(0));
    }

    #[test]
    fn resolve_unique_title_match() {
        let titles = ["Write docs", "Fix login bug"];
        assert_eq!(resolve_task_id("fix-login-bug", titles), Ok(1));
        assert_eq!(resolve_task_id("Fix Login Bug", titles), Ok(1));
    }

    #[test]
    fn resolve_title_match_ignores_case_spaces_and_slug_variants() {
        let titles = ["Write docs", "  Hello   World  "];
        assert_eq!(resolve_task_id("  HELLO   world  ", titles), Ok(1));
        assert_eq!(resolve_task_id("hello-world", titles), Ok(1));
        // 纯数字参数仍然优先，不受标题影响
        assert_eq!(resolve_task_id("0", titles), Ok(0));
    }

    #[test]
    fn resolve_ambiguous_title_returns_error() {
        let titles = ["Setup server", "setup-server", "Setup Server!"];
        assert!(matches!(
            resolve_task_id("setup-server", titles),
            Err(TaskIdError::Ambiguous { .. })
        ));
    }

    #[test]
    fn resolve_missing_title_returns_not_found() {
        let titles = ["Write docs", "Fix bug"];
        assert!(matches!(
            resolve_task_id("deploy", titles),
            Err(TaskIdError::NotFound { .. })
        ));
    }

    #[test]
    fn resolve_fuzzy_title_match_uses_contains() {
        let titles = vec!["Write documentation".to_string(), "Fix login bug".to_string()];
        assert_eq!(resolve_task_id("login", &titles), Ok(1));
        assert_eq!(resolve_task_id("documentation", &titles), Ok(0));
    }

    #[test]
    fn resolve_duplicate_title_returns_error() {
        let titles = vec!["Write docs".to_string(), " write docs ".to_string()];
        assert!(matches!(
            resolve_task_id("Write docs", &titles),
            Err(TaskIdError::DuplicateTitle { .. })
        ));
    }
}
