
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Run not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Annotation data stored in the run metadata sidecar
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Annotations {
    /// Key-value tags for the run
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tags: HashMap<String, String>,
    /// Optional free-form note
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Annotations {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_tag(mut self, key: String, value: String) -> Self {
        self.tags.insert(key, value);
        self
    }

    pub fn with_note(mut self, note: String) -> Self {
        self.note = Some(note);
        self
    }

    /// Check if annotations have any content
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.note.is_none()
    }
}

/// Get the path to the run metadata sidecar file
fn sidecar_path(runs_dir: &Path, run_id: &str) -> PathBuf {
    runs_dir.join(format!("{}.meta.json", run_id))
}

/// Load annotations from the run's metadata sidecar
pub fn load_annotations(runs_dir: &Path, run_id: &str) -> Result<Annotations> {
    let path = sidecar_path(runs_dir, run_id);
    
    if !path.exists() {
        return Ok(Annotations::default());
    }

    let content = fs::read_to_string(&path)?;
    if content.trim().is_empty() {
        return Ok(Annotations::default());
    }

    // Try to parse as Annotations directly (new format)
    if let Ok(annotations) = serde_json::from_str::<Annotations>(&content) {
        return Ok(annotations);
    }

    // Backward compatibility: try to parse as generic JSON and extract tags/note
    let value: serde_json::Value = serde_json::from_str(&content)?;
    let mut annotations = Annotations::default();
    
    if let Some(tags_obj) = value.get("tags").and_then(|v| v.as_object()) {
        for (k, v) in tags_obj {
            if let Some(s) = v.as_str() {
                annotations.tags.insert(k.clone(), s.to_string());
            }
        }
    }
    
    if let Some(note_str) = value.get("note").and_then(|v| v.as_str()) {
        annotations.note = Some(note_str.to_string());
    }

    Ok(annotations)
}

/// Save annotations to the run's metadata sidecar
pub fn save_annotations(runs_dir: &Path, run_id: &str, annotations: &Annotations) -> Result<()> {
    let path = sidecar_path(runs_dir, run_id);
    
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(annotations)?;
    fs::write(&path, content)?;
    Ok(())
}

/// Merge new tags into existing tags (new tags overwrite existing keys)
pub fn merge_tags(existing: &mut HashMap<String, String>, new_tags: HashMap<String, String>) {
    for (k, v) in new_tags {
        existing.insert(k, v);
    }
}

/// Parse tag string in format "key=value"
pub fn parse_tag(tag_str: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = tag_str.splitn(2, '=').collect();
    if parts.len() == 2 {
        Some((parts[0].to_string(), parts[1].to_string()))
    } else {
        None
    }
}

/// Filter runs by tag key and value
pub fn filter_runs_by_tag(
    runs_dir: &Path,
    run_ids: &[String],
    tag_key: &str,
    tag_value: &str,
) -> Result<Vec<String>> {
    let mut matched = Vec::new();
    
    for run_id in run_ids {
        let annotations = load_annotations(runs_dir, run_id)?;
        if let Some(value) = annotations.tags.get(tag_key) {
            if value == tag_value {
                matched.push(run_id.clone());
            }
        }
    }
    
    Ok(matched)
}

/// Filter runs by whether they have a note
pub fn filter_runs_has_note(
    runs_dir: &Path,
    run_ids: &[String],
    has_note: bool,
) -> Result<Vec<String>> {
    let mut matched = Vec::new();
    
    for run_id in run_ids {
        let annotations = load_annotations(runs_dir, run_id)?;
        let has_note_actual = annotations.note.is_some() && !annotations.note.as_ref().unwrap().is_empty();
        if has_note_actual == has_note {
            matched.push(run_id.clone());
        }
    }
    
    Ok(matched)
}

/// List all run IDs in the runs directory
pub fn list_run_ids(runs_dir: &Path) -> Result<Vec<String>> {
    let mut run_ids = Vec::new();
    
    if !runs_dir.exists() {
        return Ok(run_ids);
    }

    for entry in fs::read_dir(runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        
        // Look for .meta.json files
        if let Some(filename) = path.file_name().and_then(|f| f.to_str()) {
            if filename.ends_with(".meta.json") {
                let run_id = filename.strip_suffix(".meta.json").unwrap();
                run_ids.push(run_id.to_string());
            }
        }
    }

    run_ids.sort();
    Ok(run_ids)
}

/// Basic run information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub status: String,
    pub started_at: String,
}

/// Load all runs from the runs directory
pub fn load_runs(runs_dir: &Path) -> Result<Vec<Run>> {
    let run_ids = list_run_ids(runs_dir)?;
    let mut runs = Vec::new();
    
    for run_id in run_ids {
        // Try to load run metadata from a run.json file
        let run_path = runs_dir.join(&run_id).join("run.json");
        if run_path.exists() {
            let content = fs::read_to_string(&run_path)?;
            if let Ok(run) = serde_json::from_str::<Run>(&content) {
                runs.push(run);
                continue;
            }
        }
        
        // Fallback: create a basic run with default values
        runs.push(Run {
            id: run_id,
            status: "unknown".to_string(),
            started_at: "unknown".to_string(),
        });
    }
    
    Ok(runs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_annotations_default() {
        let ann = Annotations::default();
        assert!(ann.tags.is_empty());
        assert!(ann.note.is_none());
        assert!(ann.is_empty());
    }

    #[test]
    fn test_annotations_with_tag_and_note() {
        let ann = Annotations::new()
            .with_tag("env".to_string(), "prod".to_string())
            .with_note("Deployment successful".to_string());
        
        assert_eq!(ann.tags.get("env"), Some(&"prod".to_string()));
        assert_eq!(ann.note, Some("Deployment successful".to_string()));
        assert!(!ann.is_empty());
    }

    #[test]
    fn test_save_and_load_annotations() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path();
        
        let annotations = Annotations::new()
            .with_tag("team".to_string(), "backend".to_string())
            .with_tag("version".to_string(), "1.0.0".to_string())
            .with_note("Test run".to_string());
        
        save_annotations(runs_dir, "run-123", &annotations).unwrap();
        let loaded = load_annotations(runs_dir, "run-123").unwrap();
        
        assert_eq!(loaded.tags.get("team"), Some(&"backend".to_string()));
        assert_eq!(loaded.tags.get("version"), Some(&"1.0.0".to_string()));
        assert_eq!(loaded.note, Some("Test run".to_string()));
    }

    #[test]
    fn test_load_nonexistent_run() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path();
        
        let loaded = load_annotations(runs_dir, "nonexistent").unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_merge_tags() {
        let mut existing = HashMap::new();
        existing.insert("key1".to_string(), "value1".to_string());
        
        let mut new_tags = HashMap::new();
        new_tags.insert("key2".to_string(), "value2".to_string());
        new_tags.insert("key1".to_string(), "new_value1".to_string()); // overwrite
        
        merge_tags(&mut existing, new_tags);
        
        assert_eq!(existing.get("key1"), Some(&"new_value1".to_string()));
        assert_eq!(existing.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_parse_tag() {
        assert_eq!(parse_tag("key=value"), Some(("key".to_string(), "value".to_string())));
        assert_eq!(parse_tag("key="), Some(("key".to_string(), "".to_string())));
        assert_eq!(parse_tag("=value"), Some(("".to_string(), "value".to_string())));
        assert_eq!(parse_tag("novalue"), None);
        assert_eq!(parse_tag(""), None);
    }

    #[test]
    fn test_filter_runs_by_tag() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path();
        
        // Create runs with different tags
        save_annotations(runs_dir, "run-1", &Annotations::new().with_tag("env".to_string(), "prod".to_string())).unwrap();
        save_annotations(runs_dir, "run-2", &Annotations::new().with_tag("env".to_string(), "dev".to_string())).unwrap();
        save_annotations(runs_dir, "run-3", &Annotations::new().with_tag("team".to_string(), "backend".to_string())).unwrap();
        save_annotations(runs_dir, "run-4", &Annotations::new()).unwrap(); // no tags
        
        let run_ids = vec!["run-1".to_string(), "run-2".to_string(), "run-3".to_string(), "run-4".to_string()];
        
        let prod_runs = filter_runs_by_tag(runs_dir, &run_ids, "env", "prod").unwrap();
        assert_eq!(prod_runs, vec!["run-1"]);
        
        let dev_runs = filter_runs_by_tag(runs_dir, &run_ids, "env", "dev").unwrap();
        assert_eq!(dev_runs, vec!["run-2"]);
        
        let backend_runs = filter_runs_by_tag(runs_dir, &run_ids, "team", "backend").unwrap();
        assert_eq!(backend_runs, vec!["run-3"]);
        
        let missing_runs = filter_runs_by_tag(runs_dir, &run_ids, "env", "staging").unwrap();
        assert!(missing_runs.is_empty());
    }

    #[test]
    fn test_filter_runs_has_note() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path();
        
        save_annotations(runs_dir, "run-1", &Annotations::new().with_note("Has note".to_string())).unwrap();
        save_annotations(runs_dir, "run-2", &Annotations::new()).unwrap(); // no note
        save_annotations(runs_dir, "run-3", &Annotations::new().with_note("".to_string())).unwrap(); // empty note
        
        let run_ids = vec!["run-1".to_string(), "run-2".to_string(), "run-3".to_string()];
        
        let with_note = filter_runs_has_note(runs_dir, &run_ids, true).unwrap();
        assert_eq!(with_note, vec!["run-1"]);
        
        let without_note = filter_runs_has_note(runs_dir, &run_ids, false).unwrap();
        assert_eq!(without_note, vec!["run-2", "run-3"]);
    }

    #[test]
    fn test_list_run_ids() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path();
        
        save_annotations(runs_dir, "run-a", &Annotations::default()).unwrap();
        save_annotations(runs_dir, "run-b", &Annotations::default()).unwrap();
        save_annotations(runs_dir, "run-c", &Annotations::default()).unwrap();
        
        let ids = list_run_ids(runs_dir).unwrap();
        assert_eq!(ids, vec!["run-a", "run-b", "run-c"]);
    }

    #[test]
    fn test_backward_compatibility() {
        let dir = tempdir().unwrap();
        let runs_dir = dir.path();
        
        // Write old-format metadata (generic JSON with tags/note)
        let old_format = serde_json::json!({
            "tags": {"old_key": "old_value"},
            "note": "Old note",
            "other_field": "ignored"
        });
        fs::write(sidecar_path(runs_dir, "old-run"), serde_json::to_string_pretty(&old_format).unwrap()).unwrap();
        
        let loaded = load_annotations(runs_dir, "old-run").unwrap();
        assert_eq!(loaded.tags.get("old_key"), Some(&"old_value".to_string()));
        assert_eq!(loaded.note, Some("Old note".to_string()));
    }
}