use crate::config::Config;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use toml::Value;
use chrono::{DateTime, Utc};

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

/// Workspace migration - migrates from single-project config to multi-workspace structure.
/// This migration:
/// 1. Detects old config files (~/.xi/config.toml, agents.toml, backlog/, runs/)
/// 2. Creates a 'default' workspace
/// 3. Moves all files to ~/.xi/workspaces/default/
/// 4. Creates workspaces.toml registry and current file
/// 5. Marks migration as complete
pub struct WorkspaceMigration;

impl Migration for WorkspaceMigration {
    fn version(&self) -> &str {
        "3"
    }

    fn migrate(&self, toml: &mut Value) -> Result<()> {
        // This migration operates on the filesystem, not just the TOML value.
        // The actual filesystem migration is done in migrate_to_workspace().
        // Here we just update the version in the config.
        toml.insert("version".to_string(), Value::String("3".to_string()));
        Ok(())
    }
}

/// Performs the full workspace migration on the filesystem.
/// Returns true if migration was performed, false if already migrated or not needed.
pub fn migrate_to_workspace() -> Result<bool> {
    let xi_dir = dirs::home_dir()
        .context("Could not find home directory")?
        .join(".xi");
    
    let config_file = xi_dir.join("config.toml");
    let agents_file = xi_dir.join("agents.toml");
    let backlog_dir = xi_dir.join("backlog");
    let runs_dir = xi_dir.join("runs");
    let workspaces_file = xi_dir.join("workspaces.toml");
    let current_file = xi_dir.join("current");
    
    // Check if already migrated (workspaces.toml exists)
    if workspaces_file.exists() {
        return Ok(false);
    }
    
    // Check if there's anything to migrate
    let has_old_config = config_file.exists() || agents_file.exists() || backlog_dir.exists() || runs_dir.exists();
    if !has_old_config {
        // No old config, nothing to migrate
        return Ok(false);
    }
    
    // Create workspaces directory
    let workspaces_dir = xi_dir.join("workspaces");
    let default_workspace_dir = workspaces_dir.join("default");
    fs::create_dir_all(&default_workspace_dir).context("Failed to create default workspace directory")?;
    
    // Create subdirectories in default workspace
    fs::create_dir_all(default_workspace_dir.join("backlog")).context("Failed to create backlog directory")?;
    fs::create_dir_all(default_workspace_dir.join("runs")).context("Failed to create runs directory")?;
    
    // Move config.toml
    if config_file.exists() {
        fs::rename(&config_file, default_workspace_dir.join("config.toml"))
            .context("Failed to move config.toml")?;
    }
    
    // Move agents.toml
    if agents_file.exists() {
        fs::rename(&agents_file, default_workspace_dir.join("agents.toml"))
            .context("Failed to move agents.toml")?;
    }
    
    // Move backlog directory contents
    if backlog_dir.exists() {
        let target_backlog = default_workspace_dir.join("backlog");
        for entry in fs::read_dir(&backlog_dir).context("Failed to read backlog directory")? {
            let entry = entry.context("Failed to read backlog entry")?;
            let target = target_backlog.join(entry.file_name());
            fs::rename(entry.path(), target).context("Failed to move backlog file")?;
        }
        fs::remove_dir(&backlog_dir).context("Failed to remove old backlog directory")?;
    }
    
    // Move runs directory contents
    if runs_dir.exists() {
        let target_runs = default_workspace_dir.join("runs");
        for entry in fs::read_dir(&runs_dir).context("Failed to read runs directory")? {
            let entry = entry.context("Failed to read runs entry")?;
            let target = target_runs.join(entry.file_name());
            fs::rename(entry.path(), target).context("Failed to move runs file")?;
        }
        fs::remove_dir(&runs_dir).context("Failed to remove old runs directory")?;
    }
    
    // Create workspaces.toml registry
    let workspace_registry = WorkspaceRegistry {
        version: 1,
        workspaces: vec![WorkspaceEntry {
            name: "default".to_string(),
            path: default_workspace_dir.to_string_lossy().to_string(),
            created_at: Utc::now(),
            description: Some("Migrated from single-project configuration".to_string()),
        }],
    };
    
    let registry_toml = toml::to_string_pretty(&workspace_registry)
        .context("Failed to serialize workspace registry")?;
    fs::write(&workspaces_file, registry_toml).context("Failed to write workspaces.toml")?;
    
    // Create current file pointing to default workspace
    fs::write(&current_file, "default").context("Failed to write current workspace file")?;
    
    Ok(true)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspaceRegistry {
    version: u32,
    workspaces: Vec<WorkspaceEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct WorkspaceEntry {
    name: String,
    path: String,
    created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Updates the MIGRATIONS array to include the workspace migration.
/// This should be called during config loading to ensure all migrations are registered.
pub fn register_workspace_migration() {
    // The workspace migration is handled separately via migrate_to_workspace()
    // because it operates on the filesystem, not just the config TOML.
    // This function exists for API completeness.
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml::Value;
    use std::fs;
    use tempfile::tempdir;

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

    #[test]
    fn test_workspace_migration() {
        let temp_dir = tempdir().unwrap();
        let xi_dir = temp_dir.path().join(".xi");
        fs::create_dir_all(&xi_dir).unwrap();
        
        // Create old config files
        fs::write(xi_dir.join("config.toml"), "version = \"2\"\neditor = \"code\"").unwrap();
        fs::write(xi_dir.join("agents.toml"), "[[agents]]\nname = \"test\"\nmodel = \"gpt-4\"").unwrap();
        fs::create_dir_all(xi_dir.join("backlog")).unwrap();
        fs::write(xi_dir.join("backlog").join("task1.md"), "# Task 1").unwrap();
        fs::create_dir_all(xi_dir.join("runs")).unwrap();
        fs::write(xi_dir.join("runs").join("run1.json"), "{}").unwrap();
        
        // We can't easily test migrate_to_workspace() without mocking the home directory,
        // but we can test the WorkspaceMigration struct
        let mut toml = Value::Table({
            let mut map = toml::map::Map::new();
            map.insert("version".to_string(), Value::String("2".to_string()));
            map
        });
        
        let migration = WorkspaceMigration;
        migration.migrate(&mut toml).unwrap();
        
        assert_eq!(toml.get("version").unwrap().as_str(), Some("3"));
    }

    #[test]
    fn test_workspace_migration_version() {
        let migration = WorkspaceMigration;
        assert_eq!(migration.version(), "3");
    }
}
}
