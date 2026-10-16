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
            if ch.is_alphanumeric() {
                out.extend(ch.to_lowercase());
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

    /// 根据已有 slug 集合解决冲突：若 base 已存在，则追加短哈希。
    /// 短哈希基于 base 及尝试次数计算，确保确定性且冲突时可继续变化。
    pub fn unique_slug(base: &str, existing_slugs: &HashSet<&str>) -> String {
        if !existing_slugs.contains(base) {
            return base.to_string();
        }

        let mut attempt = 1;
        loop {
            let candidate = format!(
                "{}-{}",
                base,
                Self::short_hash(&format!("{}{}", base, attempt))
            );
            if !existing_slugs.contains(candidate.as_str()) {
                return candidate;
            }
            attempt += 1;
        }
    }

    fn short_hash(s: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hasher);
        let hex = format!("{:x}", hasher.finish());
        if hex.len() >= 6 {
            hex[..6].to_string()
        } else {
            format!("{:0>6}", hex)
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

/// 供 dev 循环使用的便捷函数：标题 → slug，并保证在已有集合中唯一。
/// 签名兼容 `HashSet<String>`，内部转换为 `HashSet<&str>` 后复用 `TaskId::unique_slug`。
pub fn unique_slug(title: &str, existing_slugs: &HashSet<String>) -> String {
    let base = TaskId::slugify(title);
    let base = if base.is_empty() {
        "task".to_string()
    } else {
        base
    };
    let borrowed: HashSet<&str> = existing_slugs.iter().map(|s| s.as_str()).collect();
    TaskId::unique_slug(&base, &borrowed)
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
    /// 未找到匹配任务，附带可用任务列表。
    NotFound { query: String, available: Vec<String> },
    /// 找到多个匹配任务，返回候选标题。
    Ambiguous { query: String, matches: Vec<String> },
    /// 找到多个匹配任务，但它们的标题完全相同。
    DuplicateTitle { query: String, title: String },

}

impl fmt::Display for TaskIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskIdError::NotFound { query, available } => {
                write!(f, "task id '{}' not found; available tasks:\n", query)?;
                for (idx, title) in available.iter().enumerate() {
                    write!(f, "  [{}] {}\n", idx, title)?;
                }
                Ok(())
            },
            TaskIdError::Ambiguous { query, matches } => {
                write!(f, "task id '{}' is ambiguous; matches: {}; use numeric index to disambiguate", query, matches.join(", "))
            }
            TaskIdError::DuplicateTitle { query, title } => {
                write!(
                    f,
                    "task id '{}' matches multiple tasks with title '{}'; use numeric index to disambiguate",
                    query,
                    title
                )
            }

        }
    }
}

impl std::error::Error for TaskIdError {}

fn normalize_title(title: &str) -> String {
    title.chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase()
}

fn duplicate_or_ambiguous(query: &str, matches: Vec<(usize, String)>) -> TaskIdError {
    let unique_titles: HashSet<String> = matches
        .iter()
        .map(|(_, title)| normalize_title(title))
        .collect();
    if unique_titles.len() == 1 {
        return TaskIdError::DuplicateTitle {
            query: query.to_string(),
            title: matches[0].1.clone(),
        };
    }

    let unique_slugs: HashSet<String> = matches
        .iter()
        .map(|(_, title)| TaskId::slugify(title))
        .filter(|slug| !slug.is_empty())
        .collect();
    if unique_slugs.len() == 1 && matches.len() > 1 {
        let match_titles = matches.into_iter().map(|(_, title)| title).collect();
        return TaskIdError::Ambiguous {
            query: query.to_string(),
            matches: match_titles,
        };
    }

    TaskIdError::Ambiguous {
        query: query.to_string(),
        matches: matches.into_iter().map(|(_, title)| title).collect(),
    }
}

/// 解析原始 task_id 参数。
///
/// 解析顺序：
/// 1. 将输入转换为 slug，与任务标题生成的 slug 精确匹配；
/// 2. 如果没有 slug 匹配，且输入可解析为数字索引，则回退到 `TaskId::Numeric`；
/// 3. 仍然没有匹配时，回退到旧模糊匹配（标题前缀或 slug 前缀）；
/// 4. 所有匹配失败返回 `NotFound`，错误中包含可用任务列表。
pub fn resolve_task_id<I, S>(raw: &str, titles: I) -> Result<TaskId, TaskIdError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let title_list: Vec<String> = titles
        .into_iter()
        .map(|s| s.as_ref().to_string())
        .collect();

    let query_slug = TaskId::slugify(raw);
    if !query_slug.is_empty() {
        let mut slug_matches = Vec::new();
        for (index, title) in title_list.iter().enumerate() {
            let title_slug = TaskId::slugify(title);
            if title_slug == query_slug {
                slug_matches.push((index, title.clone()));
            }
        }

        match slug_matches.len() {
            1 => {
                let slug = TaskId::slugify(&slug_matches[0].1);
                if slug.is_empty() {
                    return Ok(TaskId::Numeric(slug_matches[0].0));
                }
                return Ok(TaskId::Slug(slug));
            }
            0 => {}
            _ => return Err(duplicate_or_ambiguous(raw, slug_matches)),
        }
    }

    // 数字索引回退：保持旧行为，不检查索引是否存在。
    if let Ok(index) = raw.parse::<usize>() {
        return Ok(TaskId::Numeric(index));
    }

    // 旧模糊匹配作为最终回退。
    let query = normalize_title(raw);
    let query_slug = TaskId::slugify(raw);
    if query.is_empty() && query_slug.is_empty() {
        return Err(TaskIdError::NotFound {
            query: raw.to_string(),
            available: title_list,
        });
    }

    let mut fuzzy_matches: Vec<(usize, String)> = Vec::new();
    for (index, title) in title_list.iter().enumerate() {
        let normalized_title = normalize_title(title);
        let title_slug = TaskId::slugify(title);
        if normalized_title.starts_with(&query)
            || (!query_slug.is_empty() && title_slug.starts_with(&query_slug))
        {
            fuzzy_matches.push((index, title.clone()));
        }
    }

    match fuzzy_matches.len() {
        0 => Err(TaskIdError::NotFound {
            query: raw.to_string(),
            available: title_list,
        }),
        1 => {
            let slug = TaskId::slugify(&fuzzy_matches[0].1);
            if slug.is_empty() {
                return Ok(TaskId::Numeric(fuzzy_matches[0].0));
            }
            Ok(TaskId::Slug(slug))
        }
        _ => Err(duplicate_or_ambiguous(raw, fuzzy_matches)),
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
    fn from_title_resolves_conflicts_with_short_hash() {
        let mut existing = HashSet::new();
        existing.insert("hello");
        let id = TaskId::from_title("Hello", 1, &existing);
        match id {
            TaskId::Slug(slug) => {
                assert!(slug.starts_with("hello-"));
                assert_ne!(slug, "hello");
                assert!(!existing.contains(slug.as_str()));
            }
            _ => panic!("expected slug"),
        }
    }

    #[test]
    fn unique_slug_appends_short_hash() {
        let mut existing = HashSet::new();
        existing.insert("task");
        let result = TaskId::unique_slug("task", &existing);
        assert!(result.starts_with("task-"));
        assert_ne!(result, "task");
        assert!(!existing.contains(result.as_str()));

        assert_eq!(TaskId::unique_slug("brand-new", &existing), "brand-new");
    }

    #[test]
    fn resolve_numeric_index_passthrough() {
        let titles = ["Write docs", "Fix bug"];
        assert_eq!(resolve_task_id("1", titles), Ok(TaskId::Numeric(1)));
        assert_eq!(resolve_task_id("0", titles), Ok(TaskId::Numeric(0)));
    }

    #[test]
    fn resolve_unique_title_exact_match() {
        let titles = ["Write docs", "Fix login bug"];
        assert_eq!(
            resolve_task_id("fix login bug", titles),
            Ok(TaskId::Slug("fix-login-bug".to_string()))
        );
        assert_eq!(
            resolve_task_id("Write docs", titles),
            Ok(TaskId::Slug("write-docs".to_string()))
        );
        assert_eq!(
            resolve_task_id("  fix login bug  ", titles),
            Ok(TaskId::Slug("fix-login-bug".to_string()))
        );
        assert_eq!(
            resolve_task_id("FIX LOGIN BUG", titles),
            Ok(TaskId::Slug("fix-login-bug".to_string()))
        );
    }

    #[test]
    fn resolve_title_match_ignores_case_and_spaces() {
        let titles = ["Write docs", "  Hello   World  "];
        assert_eq!(
            resolve_task_id("  HELLO   ", titles),
            Ok(TaskId::Slug("hello-world".to_string()))
        );
        assert_eq!(
            resolve_task_id("hELLo", titles),
            Ok(TaskId::Slug("hello-world".to_string()))
        );
        // 纯数字参数仍然优先，不受标题影响
        assert_eq!(resolve_task_id("0", titles), Ok(TaskId::Numeric(0)));
    }

    #[test]
    fn resolve_ambiguous_title_prefix_returns_error() {
        let titles = ["Setup server", "Setup client"];
        let err = resolve_task_id("setup", titles).unwrap_err();
        assert!(matches!(err, TaskIdError::Ambiguous { .. }));
        if let TaskIdError::Ambiguous { matches, .. } = err {
            assert!(matches.iter().any(|m| m == "Setup server"));
            assert!(matches.iter().any(|m| m == "Setup client"));
        }
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
    fn resolve_duplicate_title_returns_error() {
        let titles = vec!["Write docs".to_string(), " write docs ".to_string()];
        assert!(matches!(
            resolve_task_id("Write docs", &titles),
            Err(TaskIdError::DuplicateTitle { .. })
        ));
    }

    #[test]
    fn resolve_title_ignores_internal_spaces() {
        let titles = ["Write docs", "Hello   World"];
        assert_eq!(
            resolve_task_id("Hello World", titles),
            Ok(TaskId::Slug("hello-world".to_string()))
        );
        assert_eq!(
            resolve_task_id("hello   world", titles),
            Ok(TaskId::Slug("hello-world".to_string()))
        );
        assert_eq!(
            resolve_task_id("HELLO WORLD", titles),
            Ok(TaskId::Slug("hello-world".to_string()))
        );
    }

    #[test]
    fn resolve_slug_form_matches_title() {
        let titles = ["Write docs", "Fix login bug", "Hello, World!"];
        assert_eq!(
            resolve_task_id("hello-world", titles),
            Ok(TaskId::Slug("hello-world".to_string()))
        );
        assert_eq!(
            resolve_task_id("fix-login-bug", titles),
            Ok(TaskId::Slug("fix-login-bug".to_string()))
        );
    }

    #[test]
    fn resolve_ambiguous_error_prompts_numeric_index() {
        let titles = ["Setup server", "Setup client"];
        let err = resolve_task_id("setup", titles).unwrap_err();
        assert!(err.to_string().contains("numeric index"));
    }

    #[test]
    fn resolve_slug_conflict_different_titles_returns_error() {
        let titles = ["Hello World!", "Hello World?"];
        let err = resolve_task_id("hello-world", titles).unwrap_err();
        assert!(matches!(err, TaskIdError::Ambiguous { .. }));
        assert!(err.to_string().contains("numeric index"));
    }

    #[test]
    fn resolve_slug_alias_has_priority_over_numeric_index() {
        let titles = ["Fix bug", "123"];
        // 输入 "123" 与标题 slug "123" 匹配，应返回 Slug，而不是数字索引 123
        assert_eq!(
            resolve_task_id("123", titles),
            Ok(TaskId::Slug("123".to_string()))
        );
    }

    #[test]
    fn resolve_numeric_fallback_when_no_slug_match() {
        let titles = ["Write docs", "Fix login bug"];
        assert_eq!(resolve_task_id("1", titles), Ok(TaskId::Numeric(1)));
        assert_eq!(resolve_task_id("0", titles), Ok(TaskId::Numeric(0)));
    }

    #[test]
    fn resolve_duplicate_slug_returns_ambiguous_error() {
        let titles = ["Hello World!", "Hello World?"];
        let err = resolve_task_id("hello-world", titles).unwrap_err();
        assert!(matches!(err, TaskIdError::Ambiguous { .. }));
        assert!(err.to_string().contains("numeric index"));
    }

    #[test]
    fn resolve_not_found_returns_available_tasks() {
        let titles = ["Write docs", "Fix bug"];
        let err = resolve_task_id("deploy", titles).unwrap_err();
        match &err {
            TaskIdError::NotFound { available, .. } => {
                assert_eq!(available, &vec!["Write docs".to_string(), "Fix bug".to_string()]);
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
        assert!(err.to_string().contains("available tasks"));
    }
}
