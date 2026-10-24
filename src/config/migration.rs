use crate::config::Config;
use anyhow::Result;
use std::collections::HashMap;
use toml::Value;

/// Migration trait that defines a single version migration step.
pub trait Migration: Send + Sync {
    /// Returns the target version this migration upgrades to.
    fn version(&self) -> &str;

    /// Applies the migration to the given TOML value.
    fn migrate(&self, toml: &mut Value) -> Result<()>;
}

/// Migration from v1 to v2:
/// - Renames `editor` field to `editor.command`
/// - Adds default `editor.args` as empty array
/// - Adds default `ui.theme` as "system"
/// - Restructures `git` section to include `auto_fetch` and `sign_commits`
pub struct V1ToV2;

impl Migration for V1ToV2 {
    fn version(&self) -> &str {
        "2"
    }

    fn migrate(&self, toml: &mut Value) -> Result<()> {
        // Handle editor field rename: editor -> editor.command
        if let Some(editor_value) = toml.get_mut("editor") {
            if editor_value.is_str() {
                let command = editor_value.as_str().unwrap().to_string();
                *editor_value = Value::Table({
                    let mut table = toml::map::Map::new();
                    table.insert("command".to_string(), Value::String(command));
                    table.insert("args".to_string(), Value::Array(vec![]));
                    table
                });
            }
        }

        // Ensure editor.args exists if editor is a table
        if let Some(Value::Table(editor_table)) = toml.get_mut("editor") {
            editor_table.entry("args").or_insert(Value::Array(vec![]));
        }

        // Add default ui.theme
        let ui_table = toml
            .entry("ui")
            .or_insert_with(|| Value::Table(toml::map::Map::new()));
        if let Value::Table(ui) = ui_table {
            ui.entry("theme")
                .or_insert(Value::String("system".to_string()));
        }

        // Restructure git section
        let git_table = toml
            .entry("git")
            .or_insert_with(|| Value::Table(toml::map::Map::new()));
        if let Value::Table(git) = git_table {
            git.entry("auto_fetch")
                .or_insert(Value::Bool(true));
            git.entry("sign_commits")
                .or_insert(Value::Bool(false));
        }

        // Update version field
        toml.insert("version".to_string(), Value::String("2".to_string()));

        Ok(())
    }
}

/// Version registry - migrations ordered by version.
pub static MIGRATIONS: &[&dyn Migration] = &[&V1ToV2];

/// Applies all migrations from current_version to latest.
/// Returns the final version after migrations.
pub fn apply_migrations(config: &mut Value, current_version: &str) -> Result<String> {
    let mut current = current_version.to_string();
    
    for migration in MIGRATIONS {
        let target_version = migration.version();
        if version_gt(target_version, &current) {
            migration.migrate(config)?;
            current = target_version.to_string();
        }
    }
    
    Ok(current)
}

/// Compares two semantic version strings (major.minor.patch or major).
/// Returns true if a > b.
fn version_gt(a: &str, b: &str) -> bool {
    let parse_version = |v: &str| -> Vec<u32> {
        v.split('.')
            .map(|s| s.parse().unwrap_or(0))
            .collect()
    };
    
    let a_parts = parse_version(a);
    let b_parts = parse_version(b);
    
    let max_len = a_parts.len().max(b_parts.len());
    for i in 0..max_len {
        let a_val = a_parts.get(i).copied().unwrap_or(0);
        let b_val = b_parts.get(i).copied().unwrap_or(0);
        if a_val != b_val {
            return a_val > b_val;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml::Value;

    #[test]
    fn test_v1_to_v2_migration() {
        // Simulate a v1 config
        let mut v1_config = Value::Table({
            let mut map = toml::map::Map::new();
            map.insert("version".to_string(), Value::String("1".to_string()));
            map.insert("editor".to_string(), Value::String("code".to_string()));
            map.insert("git".to_string(), Value::Table(toml::map::Map::new()));
            map
        });

        let migration = V1ToV2;
        migration.migrate(&mut v1_config).unwrap();

        // Check version updated
        assert_eq!(v1_config.get("version").unwrap().as_str(), Some("2"));

        // Check editor restructured
        let editor = v1_config.get("editor").unwrap();
        assert!(editor.is_table());
        assert_eq!(editor.get("command").unwrap().as_str(), Some("code"));
        assert_eq!(editor.get("args").unwrap().as_array(), Some(&vec![]));

        // Check ui.theme added
        let ui = v1_config.get("ui").unwrap();
        assert_eq!(ui.get("theme").unwrap().as_str(), Some("system"));

        // Check git fields added
        let git = v1_config.get("git").unwrap();
        assert_eq!(git.get("auto_fetch").unwrap().as_bool(), Some(true));
        assert_eq!(git.get("sign_commits").unwrap().as_bool(), Some(false));
    }

    #[test]
    fn test_v1_to_v2_migration_with_existing_editor_table() {
        let mut config = Value::Table({
            let mut map = toml::map::Map::new();
            map.insert("version".to_string(), Value::String("1".to_string()));
            let mut editor_table = toml::map::Map::new();
            editor_table.insert("command".to_string(), Value::String("vim".to_string()));
            map.insert("editor".to_string(), Value::Table(editor_table));
            map
        });

        V1ToV2.migrate(&mut config).unwrap();

        let editor = config.get("editor").unwrap();
        assert_eq!(editor.get("command").unwrap().as_str(), Some("vim"));
        assert_eq!(editor.get("args").unwrap().as_array(), Some(&vec![]));
    }

    #[test]
    fn test_apply_migrations_from_v1() {
        let mut config = Value::Table({
            let mut map = toml::map::Map::new();
            map.insert("version".to_string(), Value::String("1".to_string()));
            map.insert("editor".to_string(), Value::String("nano".to_string()));
            map
        });

        let final_version = apply_migrations(&mut config, "1").unwrap();
        assert_eq!(final_version, "2");
        assert_eq!(config.get("version").unwrap().as_str(), Some("2"));
    }

    #[test]
    fn test_apply_migrations_idempotent() {
        let mut config = Value::Table({
            let mut map = toml::map::Map::new();
            map.insert("version".to_string(), Value::String("2".to_string()));
            map.insert("editor".to_string(), Value::String("code".to_string()));
            map
        });

        // Running migrations on already-v2 config should not change version
        let final_version = apply_migrations(&mut config, "2").unwrap();
        assert_eq!(final_version, "2");
    }

    #[test]
    fn test_version_gt() {
        assert!(version_gt("2", "1"));
        assert!(version_gt("1.1", "1.0"));
        assert!(version_gt("2.0.0", "1.9.9"));
        assert!(!version_gt("1", "1"));
        assert!(!version_gt("1.0", "1.0.0"));
    }
}
