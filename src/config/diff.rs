//! Configuration diff algorithm
//!
//! This module provides functionality to compare two configurations
//! and produce a structured diff showing added, modified, and deleted fields.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents a single configuration change
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ConfigChange {
    /// A field was added
    Added { value: toml::Value },
    /// A field was modified
    Modified { old: toml::Value, new: toml::Value },
    /// A field was deleted
    Deleted { value: toml::Value },
}

/// Represents the diff between two configurations
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ConfigDiff {
    /// Changes at the top level or nested tables
    pub changes: HashMap<String, ConfigDiffEntry>,
}

/// A diff entry which can be a simple change or a nested table diff
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigDiffEntry {
    /// A simple value change
    Change(ConfigChange),
    /// A nested table with its own changes
    Nested(ConfigDiff),
}

impl ConfigDiff {
    /// Create a new empty diff
    pub fn new() -> Self {
        Self {
            changes: HashMap::new(),
        }
    }

    /// Check if the diff is empty (no changes)
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Compare two TOML values and produce a diff
    pub fn compare(old: &toml::Value, new: &toml::Value) -> Self {
        let mut diff = ConfigDiff::new();
        diff.compare_values("", old, new);
        diff
    }

    fn compare_values(&mut self, prefix: &str, old: &toml::Value, new: &toml::Value) {
        match (old, new) {
            (toml::Value::Table(old_table), toml::Value::Table(new_table)) => {
                self.compare_tables(prefix, old_table, new_table);
            }
            (toml::Value::Array(old_arr), toml::Value::Array(new_arr)) => {
                self.compare_arrays(prefix, old_arr, new_arr);
            }
            (old_val, new_val) if old_val != new_val => {
                let key = prefix.trim_start_matches('.').to_string();
                if key.is_empty() {
                    // Root level change - this shouldn't happen in normal usage
                    return;
                }
                self.changes.insert(
                    key,
                    ConfigDiffEntry::Change(ConfigChange::Modified {
                        old: old_val.clone(),
                        new: new_val.clone(),
                    }),
                );
            }
            _ => {
                // Values are equal, no change
            }
        }
    }

    fn compare_tables(&mut self, prefix: &str, old: &toml::map::Map<String, toml::Value>, new: &toml::map::Map<String, toml::Value>) {
        let all_keys: std::collections::HashSet<_> = old.keys().chain(new.keys()).collect();

        for key in all_keys {
            let full_key = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", prefix, key)
            };

            match (old.get(key), new.get(key)) {
                (Some(old_val), Some(new_val)) => {
                    // Key exists in both - compare values
                    match (old_val, new_val) {
                        (toml::Value::Table(old_table), toml::Value::Table(new_table)) => {
                            // Nested table - recurse
                            let mut nested_diff = ConfigDiff::new();
                            nested_diff.compare_tables(&full_key, old_table, new_table);
                            if !nested_diff.is_empty() {
                                self.changes.insert(
                                    full_key,
                                    ConfigDiffEntry::Nested(nested_diff),
                                );
                            }
                        }
                        (toml::Value::Array(old_arr), toml::Value::Array(new_arr)) => {
                            // Arrays - compare as whole for now
                            if old_arr != new_arr {
                                self.changes.insert(
                                    full_key,
                                    ConfigDiffEntry::Change(ConfigChange::Modified {
                                        old: old_val.clone(),
                                        new: new_val.clone(),
                                    }),
                                );
                            }
                        }
                        _ if old_val != new_val => {
                            // Simple value change
                            self.changes.insert(
                                full_key,
                                ConfigDiffEntry::Change(ConfigChange::Modified {
                                    old: old_val.clone(),
                                    new: new_val.clone(),
                                }),
                            );
                        }
                        _ => {
                            // Equal values - no change
                        }
                    }
                }
                (None, Some(new_val)) => {
                    // Key only in new - added
                    self.changes.insert(
                        full_key,
                        ConfigDiffEntry::Change(ConfigChange::Added {
                            value: new_val.clone(),
                        }),
                    );
                }
                (Some(old_val), None) => {
                    // Key only in old - deleted
                    self.changes.insert(
                        full_key,
                        ConfigDiffEntry::Change(ConfigChange::Deleted {
                            value: old_val.clone(),
                        }),
                    );
                }
                (None, None) => unreachable!(),
            }
        }
    }

    fn compare_arrays(&mut self, prefix: &str, old: &[toml::Value], new: &[toml::Value]) {
        // For arrays, we do a simple comparison for now
        // In the future, this could be enhanced with proper array diffing
        if old != new {
            let key = prefix.trim_start_matches('.').to_string();
            if !key.is_empty() {
                self.changes.insert(
                    key,
                    ConfigDiffEntry::Change(ConfigChange::Modified {
                        old: toml::Value::Array(old.to_vec()),
                        new: toml::Value::Array(new.to_vec()),
                    }),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml::Value;

    fn make_table(pairs: Vec<(&str, Value)>) -> Value {
        Value::Table(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }

    #[test]
    fn test_identical_configs_empty_diff() {
        let config = make_table(vec![
            ("app.name", Value::String("test".to_string())),
            ("app.version", Value::String("1.0.0".to_string())),
            ("server.port", Value::Integer(8080)),
            ("server.host", Value::String("localhost".to_string())),
            ("features.enabled", Value::Boolean(true)),
        ]);

        let diff = ConfigDiff::compare(&config, &config);
        assert!(diff.is_empty(), "Identical configs should produce empty diff");
    }

    #[test]
    fn test_added_field() {
        let old = make_table(vec![
            ("app.name", Value::String("test".to_string())),
        ]);

        let new = make_table(vec![
            ("app.name", Value::String("test".to_string())),
            ("app.new_field", Value::String("added".to_string())),
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        assert!(!diff.is_empty());
        assert_eq!(diff.changes.len(), 1);

        let change = diff.changes.get("app.new_field").expect("Should have added field");
        match change {
            ConfigDiffEntry::Change(ConfigChange::Added { value }) => {
                assert_eq!(*value, Value::String("added".to_string()));
            }
            _ => panic!("Expected Added change, got {:?}", change),
        }
    }

    #[test]
    fn test_modified_field() {
        let old = make_table(vec![
            ("app.name", Value::String("old_name".to_string())),
            ("server.port", Value::Integer(8080)),
        ]);

        let new = make_table(vec![
            ("app.name", Value::String("new_name".to_string())),
            ("server.port", Value::Integer(8080)),
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        assert!(!diff.is_empty());
        assert_eq!(diff.changes.len(), 1);

        let change = diff.changes.get("app.name").expect("Should have modified field");
        match change {
            ConfigDiffEntry::Change(ConfigChange::Modified { old, new }) => {
                assert_eq!(*old, Value::String("old_name".to_string()));
                assert_eq!(*new, Value::String("new_name".to_string()));
            }
            _ => panic!("Expected Modified change, got {:?}", change),
        }
    }

    #[test]
    fn test_deleted_field() {
        let old = make_table(vec![
            ("app.name", Value::String("test".to_string())),
            ("app.to_delete", Value::String("will_be_removed".to_string())),
        ]);

        let new = make_table(vec![
            ("app.name", Value::String("test".to_string())),
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        assert!(!diff.is_empty());
        assert_eq!(diff.changes.len(), 1);

        let change = diff.changes.get("app.to_delete").expect("Should have deleted field");
        match change {
            ConfigDiffEntry::Change(ConfigChange::Deleted { value }) => {
                assert_eq!(*value, Value::String("will_be_removed".to_string()));
            }
            _ => panic!("Expected Deleted change, got {:?}", change),
        }
    }

    #[test]
    fn test_nested_table_changes() {
        let old = make_table(vec![
            ("database", make_table(vec![
                ("host", Value::String("localhost".to_string())),
                ("port", Value::Integer(5432)),
            ])),
            ("cache", make_table(vec![
                ("enabled", Value::Boolean(true)),
            ])),
        ]);

        let new = make_table(vec![
            ("database", make_table(vec![
                ("host", Value::String("db.example.com".to_string())),
                ("port", Value::Integer(5432)),
                ("username", Value::String("admin".to_string())),
            ])),
            ("cache", make_table(vec![
                ("enabled", Value::Boolean(false)),
            ])),
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        assert!(!diff.is_empty());

        // Check database nested changes
        let db_change = diff.changes.get("database").expect("Should have database changes");
        match db_change {
            ConfigDiffEntry::Nested(nested_diff) => {
                assert_eq!(nested_diff.changes.len(), 2);
                
                // Modified host
                let host_change = nested_diff.changes.get("database.host").expect("Should have host change");
                match host_change {
                    ConfigDiffEntry::Change(ConfigChange::Modified { old, new }) => {
                        assert_eq!(*old, Value::String("localhost".to_string()));
                        assert_eq!(*new, Value::String("db.example.com".to_string()));
                    }
                    _ => panic!("Expected Modified for host"),
                }

                // Added username
                let user_change = nested_diff.changes.get("database.username").expect("Should have username added");
                match user_change {
                    ConfigDiffEntry::Change(ConfigChange::Added { value }) => {
                        assert_eq!(*value, Value::String("admin".to_string()));
                    }
                    _ => panic!("Expected Added for username"),
                }
            }
            _ => panic!("Expected Nested diff for database"),
        }

        // Check cache nested changes
        let cache_change = diff.changes.get("cache").expect("Should have cache changes");
        match cache_change {
            ConfigDiffEntry::Nested(nested_diff) => {
                assert_eq!(nested_diff.changes.len(), 1);
                let enabled_change = nested_diff.changes.get("cache.enabled").expect("Should have enabled change");
                match enabled_change {
                    ConfigDiffEntry::Change(ConfigChange::Modified { old, new }) => {
                        assert_eq!(*old, Value::Boolean(true));
                        assert_eq!(*new, Value::Boolean(false));
                    }
                    _ => panic!("Expected Modified for cache.enabled"),
                }
            }
            _ => panic!("Expected Nested diff for cache"),
        }
    }

    #[test]
    fn test_array_changes() {
        let old = make_table(vec![
            ("servers", Value::Array(vec![
                Value::String("server1".to_string()),
                Value::String("server2".to_string()),
            ])),
            ("ports", Value::Array(vec![
                Value::Integer(8080),
                Value::Integer(8081),
            ])),
        ]);

        let new = make_table(vec![
            ("servers", Value::Array(vec![
                Value::String("server1".to_string()),
                Value::String("server2".to_string()),
                Value::String("server3".to_string()),
            ])),
            ("ports", Value::Array(vec![
                Value::Integer(8080),
                Value::Integer(9090),
            ])),
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        assert!(!diff.is_empty());
        assert_eq!(diff.changes.len(), 2);

        // Check servers array modified
        let servers_change = diff.changes.get("servers").expect("Should have servers change");
        match servers_change {
            ConfigDiffEntry::Change(ConfigChange::Modified { old, new }) => {
                assert_eq!(*old, Value::Array(vec![
                    Value::String("server1".to_string()),
                    Value::String("server2".to_string()),
                ]));
                assert_eq!(*new, Value::Array(vec![
                    Value::String("server1".to_string()),
                    Value::String("server2".to_string()),
                    Value::String("server3".to_string()),
                ]));
            }
            _ => panic!("Expected Modified for servers array"),
        }

        // Check ports array modified
        let ports_change = diff.changes.get("ports").expect("Should have ports change");
        match ports_change {
            ConfigDiffEntry::Change(ConfigChange::Modified { old, new }) => {
                assert_eq!(*old, Value::Array(vec![
                    Value::Integer(8080),
                    Value::Integer(8081),
                ]));
                assert_eq!(*new, Value::Array(vec![
                    Value::Integer(8080),
                    Value::Integer(9090),
                ]));
            }
            _ => panic!("Expected Modified for ports array"),
        }
    }

    #[test]
    fn test_mixed_changes() {
        let old = make_table(vec![
            ("app.name", Value::String("old".to_string())),
            ("app.version", Value::String("1.0".to_string())),
            ("database.host", Value::String("localhost".to_string())),
            ("database.port", Value::Integer(5432)),
            ("feature.old_flag", Value::Boolean(true)),
        ]);

        let new = make_table(vec![
            ("app.name", Value::String("new".to_string())),        // modified
            ("app.version", Value::String("1.0".to_string())),      // unchanged
            ("app.new_field", Value::String("added".to_string())),  // added
            ("database.host", Value::String("db.example.com".to_string())), // modified
            ("database.port", Value::Integer(5432)),                 // unchanged
            // feature.old_flag deleted
            ("feature.new_flag", Value::Boolean(false)),              // added
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        assert_eq!(diff.changes.len(), 4);

        // Verify each change type
        assert!(matches!(
            diff.changes.get("app.name"),
            Some(ConfigDiffEntry::Change(ConfigChange::Modified { .. }))
        ));
        assert!(matches!(
            diff.changes.get("app.new_field"),
            Some(ConfigDiffEntry::Change(ConfigChange::Added { .. }))
        ));
        assert!(matches!(
            diff.changes.get("database.host"),
            Some(ConfigDiffEntry::Change(ConfigChange::Modified { .. }))
        ));
        assert!(matches!(
            diff.changes.get("feature.old_flag"),
            Some(ConfigDiffEntry::Change(ConfigChange::Deleted { .. }))
        ));
        assert!(matches!(
            diff.changes.get("feature.new_flag"),
            Some(ConfigDiffEntry::Change(ConfigChange::Added { .. }))
        ));
    }

    #[test]
    fn test_empty_vs_populated() {
        let old = Value::Table(toml::map::Map::new());
        let new = make_table(vec![
            ("app.name", Value::String("test".to_string())),
            ("server.port", Value::Integer(8080)),
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        assert_eq!(diff.changes.len(), 2);
        assert!(matches!(
            diff.changes.get("app.name"),
            Some(ConfigDiffEntry::Change(ConfigChange::Added { .. }))
        ));
        assert!(matches!(
            diff.changes.get("server.port"),
            Some(ConfigDiffEntry::Change(ConfigChange::Added { .. }))
        ));
    }

    #[test]
    fn test_populated_vs_empty() {
        let old = make_table(vec![
            ("app.name", Value::String("test".to_string())),
            ("server.port", Value::Integer(8080)),
        ]);
        let new = Value::Table(toml::map::Map::new());

        let diff = ConfigDiff::compare(&old, &new);
        assert_eq!(diff.changes.len(), 2);
        assert!(matches!(
            diff.changes.get("app.name"),
            Some(ConfigDiffEntry::Change(ConfigChange::Deleted { .. }))
        ));
        assert!(matches!(
            diff.changes.get("server.port"),
            Some(ConfigDiffEntry::Change(ConfigChange::Deleted { .. }))
        ));
    }

    #[test]
    fn test_deeply_nested_changes() {
        let old = make_table(vec![
            ("level1", make_table(vec![
                ("level2", make_table(vec![
                    ("level3", make_table(vec![
                        ("value", Value::String("deep".to_string())),
                    ])),
                ])),
            ])),
        ]);

        let new = make_table(vec![
            ("level1", make_table(vec![
                ("level2", make_table(vec![
                    ("level3", make_table(vec![
                        ("value", Value::String("modified".to_string())),
                        ("new_field", Value::Integer(42)),
                    ])),
                ])),
            ])),
        ]);

        let diff = ConfigDiff::compare(&old, &new);
        
        // Should have nested diff at level1
        let l1 = diff.changes.get("level1").expect("Should have level1");
        match l1 {
            ConfigDiffEntry::Nested(d1) => {
                let l2 = d1.changes.get("level1.level2").expect("Should have level2");
                match l2 {
                    ConfigDiffEntry::Nested(d2) => {
                        let l3 = d2.changes.get("level1.level2.level3").expect("Should have level3");
                        match l3 {
                            ConfigDiffEntry::Nested(d3) => {
                                assert_eq!(d3.changes.len(), 2);
                                assert!(matches!(
                                    d3.changes.get("level1.level2.level3.value"),
                                    Some(ConfigDiffEntry::Change(ConfigChange::Modified { .. }))
                                ));
                                assert!(matches!(
                                    d3.changes.get("level1.level2.level3.new_field"),
                                    Some(ConfigDiffEntry::Change(ConfigChange::Added { .. }))
                                ));
                            }
                            _ => panic!("Expected Nested at level3"),
                        }
                    }
                    _ => panic!("Expected Nested at level2"),
                }
            }
            _ => panic!("Expected Nested at level1"),
        }
    }
}
