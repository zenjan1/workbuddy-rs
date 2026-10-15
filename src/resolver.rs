//! Backwards-compatible task ID resolver.
//!
//! Accepts either a numeric index (existing behavior) or a task title/slug,
//! resolving to a zero-based index into `items`.

use std::fmt;

/// Errors produced by task ID resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// No task matched the provided input.
    NotFound,
    /// Multiple tasks matched the provided input.
    Ambiguous(Vec<usize>),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "task not found"),
            Self::Ambiguous(indices) => {
                write!(f, "ambiguous task selection (matches: {indices:?})")
            }
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolve a task selector against `items`.
///
/// `title` extracts the human-readable title for each item. Resolution is
/// intentionally strict and ordered to remain backwards-compatible:
///
/// 1. If `input` parses as a `usize`, it is treated as a zero-based numeric
///    index. Valid indices are returned immediately, preserving the existing
///    CLI numeric-index behavior. An out-of-range numeric input is reported as
///    `NotFound` rather than falling through to title matching.
/// 2. Exact title match, case-insensitive. Duplicate exact titles are
///    ambiguous.
/// 3. Exact slug match. The slug normalizes by lowercasing, trimming, and
///    replacing non-alphanumeric characters with hyphens.
/// 4. Fuzzy unique match: first by case-insensitive substring, then by small
///    edit distance.
pub fn resolve_task_id<T, F>(
    items: &[T],
    input: &str,
    title: F,
) -> Result<usize, ResolveError>
where
    F: Fn(&T) -> &str,
{
    let trimmed = input.trim();

    // An empty selector can never resolve to a task.
    if trimmed.is_empty() {
        return Err(ResolveError::NotFound);
    }

    // 1. Preserve numeric index semantics.
    if let Ok(idx) = trimmed.parse::<usize>() {
        return if idx < items.len() {
            Ok(idx)
        } else {
            Err(ResolveError::NotFound)
        };
    }

    if items.is_empty() {
        return Err(ResolveError::NotFound);
    }

    // 2. Case-insensitive exact title match.
    let input_lower = trimmed.to_lowercase();
    let exact_matches: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| title(item).trim().to_lowercase() == input_lower)
        .map(|(i, _)| i)
        .collect();

    match exact_matches.len() {
        1 => return Ok(exact_matches[0]),
        n if n > 1 => return Err(ResolveError::Ambiguous(exact_matches)),
        _ => {}
    }

    // 3. Exact slug match.
    let input_slug = to_slug(trimmed);
    if !input_slug.is_empty() {
        let slug_matches: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| to_slug(title(item)) == input_slug)
            .map(|(i, _)| i)
            .collect();

        match slug_matches.len() {
            1 => return Ok(slug_matches[0]),
            n if n > 1 => return Err(ResolveError::Ambiguous(slug_matches)),
            _ => {}
        }
    }

    // 4. Fuzzy unique match.
    fuzzy_match(items, &input_lower, title)
}

fn to_slug(s: &str) -> String {
    let mut slug = String::new();
    let mut last_was_hyphen = false;

    for ch in s.trim().to_lowercase().chars() {
        if ch.is_alphanumeric() {
            slug.push(ch);
            last_was_hyphen = false;
        } else if !last_was_hyphen {
            slug.push('-');
            last_was_hyphen = true;
        }
    }

    while slug.ends_with('-') {
        slug.pop();
    }

    slug
}

fn fuzzy_match<T, F>(
    items: &[T],
    input_lower: &str,
    title: F,
) -> Result<usize, ResolveError>
where
    F: Fn(&T) -> &str,
{
    if input_lower.is_empty() {
        return Err(ResolveError::NotFound);
    }

    // Case-insensitive substring matches. Accept either `input` being a
    // substring of a title or a title being a substring of `input` to be
    // lenient with long selectors.
    let substring_matches: Vec<usize> = items
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            let title_lower = title(item).trim().to_lowercase();
            title_lower.contains(input_lower) || input_lower.contains(title_lower.as_str())
        })
        .map(|(i, _)| i)
        .collect();

    match substring_matches.len() {
        1 => return Ok(substring_matches[0]),
        n if n > 1 => return Err(ResolveError::Ambiguous(substring_matches)),
        _ => {}
    }

    // Small edit-distance fallback.
    let input_len = input_lower.chars().count();
    let max_distance = if input_len <= 4 { 1 } else { 2 };

    let mut best_distance = usize::MAX;
    let mut best_indices = Vec::new();

    for (idx, item) in items.iter().enumerate() {
        let title_lower = title(item).trim().to_lowercase();
        let distance = levenshtein(input_lower, &title_lower);
        if distance < best_distance {
            best_distance = distance;
            best_indices.clear();
            best_indices.push(idx);
        } else if distance == best_distance {
            best_indices.push(idx);
        }
    }

    if best_distance <= max_distance {
        match best_indices.len() {
            1 => return Ok(best_indices[0]),
            n if n > 1 => return Err(ResolveError::Ambiguous(best_indices)),
            _ => {}
        }
    }

    Err(ResolveError::NotFound)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0usize; b_chars.len() + 1];

    for i in 1..=a_chars.len() {
        curr[0] = i;
        for j in 1..=b_chars.len() {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_chars.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Task {
        title: &'static str,
    }

    fn title(t: &Task) -> &str {
        t.title
    }

    fn ids(items: &[Task], input: &str) -> Result<usize, ResolveError> {
        resolve_task_id(items, input, title)
    }

    #[test]
    fn numeric_index_still_works() {
        let tasks = [Task { title: "write code" }, Task { title: "write tests" }];
        assert_eq!(ids(&tasks, "0"), Ok(0));
        assert_eq!(ids(&tasks, "1"), Ok(1));
        assert_eq!(ids(&tasks, " 1 "), Ok(1));
        assert_eq!(ids(&tasks, "2"), Err(ResolveError::NotFound));
    }

    #[test]
    fn exact_title_match_is_case_insensitive() {
        let tasks = [Task { title: "Write Code" }];
        assert_eq!(ids(&tasks, "write code"), Ok(0));
        assert_eq!(ids(&tasks, "  WRITE CODE  "), Ok(0));
    }

    #[test]
    fn exact_duplicate_titles_are_ambiguous() {
        let tasks = [Task { title: "duplicate" }, Task { title: "duplicate" }];
        assert_eq!(ids(&tasks, "duplicate"), Err(ResolveError::Ambiguous(vec![0, 1])));
    }

    #[test]
    fn fuzzy_multiple_ambiguous() {
        let tasks = [
            Task { title: "user login" },
            Task { title: "user logout" },
            Task { title: "admin login" },
        ];
        assert_eq!(ids(&tasks, "user"), Err(ResolveError::Ambiguous(vec![0, 1])));
    }

    #[test]
    fn fuzzy_unique_substring_match() {
        let tasks = [
            Task { title: "user login" },
            Task { title: "user logout" },
            Task { title: "admin account" },
        ];
        assert_eq!(ids(&tasks, "admin"), Ok(2));
    }

    #[test]
    fn fuzzy_unique_edit_distance_match() {
        let tasks = [Task { title: "write code" }, Task { title: "read tests" }];
        assert_eq!(ids(&tasks, "write cod"), Ok(0));
    }

    #[test]
    fn nonexistent_input_returns_not_found() {
        let tasks = [Task { title: "write code" }];
        assert_eq!(ids(&tasks, "does not exist"), Err(ResolveError::NotFound));
    }

    #[test]
    fn slug_matching_works() {
        let tasks = [
            Task { title: "Write Code!" },
            Task { title: "Write Tests" },
        ];
        assert_eq!(ids(&tasks, "write-code"), Ok(0));
    }

    #[test]
    fn slug_duplicate_titles_are_ambiguous() {
        let tasks = [
            Task { title: "Write Code!" },
            Task { title: "Write Code?" },
        ];
        assert_eq!(ids(&tasks, "write-code"), Err(ResolveError::Ambiguous(vec![0, 1])));
    }
}
