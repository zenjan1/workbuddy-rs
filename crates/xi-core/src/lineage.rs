//! Lineage tracking module for Agent genealogy and impact analysis.
//!
//! This module provides functionality to build Directed Acyclic Graphs (DAGs) of agent spawns,
//! render them in various formats (tree, DOT, JSON), and compute impact statistics.

use crate::storage::{Storage, RunRecord, AgentSpawnEvent};
use crate::error::Result;
use std::collections::{HashMap, HashSet, VecDeque};
use serde::{Serialize, Deserialize};

/// Represents a node in the agent lineage DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageNode {
    /// Unique agent identifier
    pub agent_id: String,
    /// Human-readable agent name
    pub agent_name: String,
    /// Parent agent ID (None for root)
    pub parent_agent_id: Option<String>,
    /// Reason this agent was spawned
    pub spawn_reason: String,
    /// Total tokens consumed by this agent
    pub total_tokens: u64,
    /// Current status of the agent run
    pub status: String,
    /// Start timestamp
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// End timestamp
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Child agent IDs
    pub children: Vec<String>,
}

/// Represents an edge in the lineage DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageEdge {
    /// Parent agent ID
    pub from: String,
    /// Child agent ID
    pub to: String,
    /// Reason for the spawn
    pub spawn_reason: String,
}

/// The complete DAG structure for a run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineageDag {
    /// Run ID this DAG belongs to
    pub run_id: String,
    /// All nodes in the DAG, keyed by agent_id
    pub nodes: HashMap<String, LineageNode>,
    /// All edges in the DAG
    pub edges: Vec<LineageEdge>,
    /// Root agent IDs (agents with no parent)
    pub roots: Vec<String>,
}

/// Statistics for agent impact analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactStats {
    /// Agent name analyzed
    pub agent_name: String,
    /// Total number of descendant tasks (including indirect)
    pub subtask_count: usize,
    /// Total tokens consumed by this agent and all descendants
    pub total_tokens: u64,
    /// Failure rate among descendants (0.0 to 1.0)
    pub failure_rate: f64,
    /// Number of failed descendant tasks
    pub failed_count: usize,
    /// Number of successful descendant tasks
    pub success_count: usize,
}

/// Output format for lineage rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// ASCII tree format
    Tree,
    /// GraphViz DOT format
    Dot,
    /// JSON format
    Json,
}

impl LineageDag {
    /// Create a new empty DAG for a run.
    pub fn new(run_id: String) -> Self {
        Self {
            run_id,
            nodes: HashMap::new(),
            edges: Vec::new(),
            roots: Vec::new(),
        }
    }

    /// Add a node to the DAG.
    pub fn add_node(&mut self, node: LineageNode) {
        let agent_id = node.agent_id.clone();
        let is_root = node.parent_agent_id.is_none();
        self.nodes.insert(agent_id.clone(), node);
        if is_root {
            self.roots.push(agent_id);
        }
    }

    /// Add an edge to the DAG.
    pub fn add_edge(&mut self, edge: LineageEdge) {
        // Update parent's children list
        if let Some(parent) = self.nodes.get_mut(&edge.from) {
            if !parent.children.contains(&edge.to) {
                parent.children.push(edge.to.clone());
            }
        }
        self.edges.push(edge);
    }

    /// Get a node by agent_id.
    pub fn get_node(&self, agent_id: &str) -> Option<&LineageNode> {
        self.nodes.get(agent_id)
    }

    /// Get all descendant agent IDs for a given agent (including indirect).
    pub fn get_descendants(&self, agent_id: &str) -> Vec<String> {
        let mut descendants = Vec::new();
        let mut queue = VecDeque::new();
        
        if let Some(node) = self.nodes.get(agent_id) {
            for child_id in &node.children {
                queue.push_back(child_id.clone());
            }
        }

        while let Some(current_id) = queue.pop_front() {
            if !descendants.contains(&current_id) {
                descendants.push(current_id.clone());
                if let Some(node) = self.nodes.get(&current_id) {
                    for child_id in &node.children {
                        queue.push_back(child_id.clone());
                    }
                }
            }
        }

        descendants
    }

    /// Get all ancestor agent IDs for a given agent.
    pub fn get_ancestors(&self, agent_id: &str) -> Vec<String> {
        let mut ancestors = Vec::new();
        let mut current_id = agent_id.to_string();

        while let Some(node) = self.nodes.get(&current_id) {
            if let Some(parent_id) = &node.parent_agent_id {
                ancestors.push(parent_id.clone());
                current_id = parent_id.clone();
            } else {
                break;
            }
        }

        ancestors
    }

    /// Render the DAG as an ASCII tree.
    pub fn render_tree(&self) -> String {
        let mut output = String::new();
        output.push_str(&format!("Lineage DAG for run: {}\n", self.run_id));
        output.push_str("==========================================\n\n");

        for (i, root_id) in self.roots.iter().enumerate() {
            let is_last_root = i == self.roots.len() - 1;
            self.render_node_tree(root_id, "", is_last_root, &mut output);
        }

        output
    }

    fn render_node_tree(&self, agent_id: &str, prefix: &str, is_last: bool, output: &mut String) {
        let node = match self.nodes.get(agent_id) {
            Some(n) => n,
            None => return,
        };

        let connector = if is_last { "└── " } else { "├── " };
        let status_icon = match node.status.as_str() {
            "completed" => "✓",
            "failed" => "✗",
            "running" => "⟳",
            _ => "?",
        };

        output.push_str(&format!(
            "{}{} {} ({}) {} tokens {}\n",
            prefix,
            connector,
            node.agent_name,
            node.agent_id,
            node.total_tokens,
            status_icon
        ));

        let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
        
        for (i, child_id) in node.children.iter().enumerate() {
            let is_last_child = i == node.children.len() - 1;
            self.render_node_tree(child_id, &child_prefix, is_last_child, output);
        }
    }

    /// Render the DAG as GraphViz DOT format.
    pub fn render_dot(&self) -> String {
        let mut dot = String::new();
        dot.push_str(&format!("digraph lineage_{} {{\n", self.run_id.replace('-', "_")));
        dot.push_str("    rankdir=TB;\n");
        dot.push_str("    node [shape=box, style=filled, fontname=\"Arial\" ];\n");
        dot.push_str("    edge [fontname=\"Arial\" ];\n\n");

        // Define nodes
        for (agent_id, node) in &self.nodes {
            let (fill_color, font_color) = match node.status.as_str() {
                "completed" => ("#d4edda", "#155724"),
                "failed" => ("#f8d7da", "#721c24"),
                "running" => ("#fff3cd", "#856404"),
                _ => ("#e2e3e5", "#383d41"),
            };

            let label = format!(
                "{} ({})\nTokens: {}\nStatus: {}\nReason: {}",
                node.agent_name,
                agent_id,
                node.total_tokens,
                node.status,
                escape_dot(&node.spawn_reason)
            );

            dot.push_str(&format!(
                "    \"{}\" [label=\"{}\", fillcolor=\"{}\", fontcolor=\"{}\"];\n",
                agent_id, label, fill_color, font_color
            ));
        }

        dot.push_str("\n");

        // Define edges
        for edge in &self.edges {
            dot.push_str(&format!(
                "    \"{}\" -> \"{}\" [label=\"{}\"];\n",
                edge.from, edge.to, escape_dot(&edge.spawn_reason)
            ));
        }

        dot.push_str("}\n");
        dot
    }

    /// Render the DAG as JSON.
    pub fn render_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self)
            .map_err(|e| crate::error::Error::Serialization(e.to_string()))
    }
}

/// Escape special characters for DOT format.
fn escape_dot(s: &str) -> String {
    s.replace('\"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Build the lineage DAG for a given run ID.
pub async fn build_dag<S: Storage>(storage: &S, run_id: &str) -> Result<LineageDag> {
    let mut dag = LineageDag::new(run_id.to_string());

    // Fetch all run records for this run_id
    let run_records = storage.get_runs_by_parent_run_id(run_id).await?;
    
    // Also get the root run record
    if let Some(root_record) = storage.get_run_record(run_id).await? {
        let node = run_record_to_node(&root_record);
        dag.add_node(node);
    }

    // Add all child run records
    for record in run_records {
        let node = run_record_to_node(&record);
        dag.add_node(node);
    }

    // Fetch spawn events to build edges
    let spawn_events = storage.get_spawn_events_by_run_id(run_id).await?;
    
    for event in spawn_events {
        if let Some(parent_id) = &event.parent_agent_id {
            let edge = LineageEdge {
                from: parent_id.clone(),
                to: event.agent_id.clone(),
                spawn_reason: event.spawn_reason.clone(),
            };
            dag.add_edge(edge);
        }
    }

    // Also check for spawn events in child runs
    for record in &dag.nodes.values().cloned().collect::<Vec<_>>() {
        if record.agent_id != run_id {
            let child_events = storage.get_spawn_events_by_run_id(&record.agent_id).await?;
            for event in child_events {
                if let Some(parent_id) = &event.parent_agent_id {
                    let edge = LineageEdge {
                        from: parent_id.clone(),
                        to: event.agent_id.clone(),
                        spawn_reason: event.spawn_reason.clone(),
                    };
                    dag.add_edge(edge);
                }
            }
        }
    }

    Ok(dag)
}

/// Convert a RunRecord to a LineageNode.
fn run_record_to_node(record: &RunRecord) -> LineageNode {
    LineageNode {
        agent_id: record.run_id.clone(),
        agent_name: record.agent_name.clone(),
        parent_agent_id: record.parent_agent_id.clone(),
        spawn_reason: record.spawn_reason.clone().unwrap_or_default(),
        total_tokens: record.total_tokens.unwrap_or(0),
        status: record.status.clone().unwrap_or_else(|| "unknown".to_string()),
        started_at: record.started_at,
        finished_at: record.finished_at,
        children: Vec::new(),
    }
}

/// Compute impact statistics for a given agent name across all runs.
pub async fn impact_stats<S: Storage>(storage: &S, agent_name: &str) -> Result<ImpactStats> {
    // Find all runs with this agent name
    let runs = storage.get_runs_by_agent_name(agent_name).await?;
    
    let mut total_subtasks = 0;
    let mut total_tokens = 0u64;
    let mut failed_count = 0;
    let mut success_count = 0;
    let mut processed_runs = HashSet::new();

    for run in runs {
        if processed_runs.contains(&run.run_id) {
            continue;
        }
        processed_runs.insert(run.run_id.clone());

        // Build DAG for this run
        let dag = build_dag(storage, &run.run_id).await?;

        // Find the node matching the agent_name (could be multiple in one run)
        for (agent_id, node) in &dag.nodes {
            if node.agent_name == agent_name {
                // Count this node's tokens
                total_tokens += node.total_tokens;

                // Count descendants
                let descendants = dag.get_descendants(agent_id);
                total_subtasks += descendants.len();

                // Check status of this node and descendants
                if node.status == "failed" {
                    failed_count += 1;
                } else if node.status == "completed" {
                    success_count += 1;
                }

                for desc_id in descendants {
                    if let Some(desc_node) = dag.get_node(&desc_id) {
                        total_tokens += desc_node.total_tokens;
                        if desc_node.status == "failed" {
                            failed_count += 1;
                        } else if desc_node.status == "completed" {
                            success_count += 1;
                        }
                    }
                }
            }
        }
    }

    let total_completed = failed_count + success_count;
    let failure_rate = if total_completed > 0 {
        failed_count as f64 / total_completed as f64
    } else {
        0.0
    };

    Ok(ImpactStats {
        agent_name: agent_name.to_string(),
        subtask_count: total_subtasks,
        total_tokens,
        failure_rate,
        failed_count,
        success_count,
    })
}

/// Render lineage for a run in the specified format.
pub async fn render_lineage<S: Storage>(
    storage: &S,
    run_id: &str,
    format: OutputFormat,
) -> Result<String> {
    let dag = build_dag(storage, run_id).await?;

    match format {
        OutputFormat::Tree => Ok(dag.render_tree()),
        OutputFormat::Dot => Ok(dag.render_dot()),
        OutputFormat::Json => dag.render_json(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use crate::RunRecord;
    use chrono::Utc;

    fn create_test_run_record(
        run_id: &str,
        agent_name: &str,
        parent_agent_id: Option<String>,
        spawn_reason: &str,
        total_tokens: u64,
        status: &str,
    ) -> RunRecord {
        RunRecord {
            run_id: run_id.to_string(),
            agent_name: agent_name.to_string(),
            parent_agent_id,
            spawn_reason: Some(spawn_reason.to_string()),
            total_tokens: Some(total_tokens),
            status: Some(status.to_string()),
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            ..Default::default()
        }
    }

    fn create_test_spawn_event(
        run_id: &str,
        agent_id: &str,
        parent_agent_id: Option<String>,
        spawn_reason: &str,
    ) -> AgentSpawnEvent {
        AgentSpawnEvent {
            run_id: run_id.to_string(),
            agent_id: agent_id.to_string(),
            parent_agent_id,
            spawn_reason: spawn_reason.to_string(),
            timestamp: Utc::now(),
            metadata: None,
        }
    }

    #[tokio::test]
    async fn test_build_dag_simple() {
        let storage = MemoryStorage::new();
        
        // Create root run
        let root = create_test_run_record(
            "run-1", "root-agent", None, "user_request", 100, "completed"
        );
        storage.save_run_record(&root).await.unwrap();

        // Create child run
        let child = create_test_run_record(
            "run-2", "child-agent", Some("run-1".to_string()), "task_decomposition", 50, "completed"
        );
        storage.save_run_record(&child).await.unwrap();

        // Create spawn event
        let event = create_test_spawn_event(
            "run-1", "run-2", Some("run-1".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event).await.unwrap();

        let dag = build_dag(&storage, "run-1").await.unwrap();

        assert_eq!(dag.run_id, "run-1");
        assert_eq!(dag.nodes.len(), 2);
        assert_eq!(dag.edges.len(), 1);
        assert_eq!(dag.roots.len(), 1);
        assert_eq!(dag.roots[0], "run-1");
        
        let root_node = dag.get_node("run-1").unwrap();
        assert_eq!(root_node.agent_name, "root-agent");
        assert_eq!(root_node.children.len(), 1);
        assert_eq!(root_node.children[0], "run-2");

        let child_node = dag.get_node("run-2").unwrap();
        assert_eq!(child_node.agent_name, "child-agent");
        assert_eq!(child_node.parent_agent_id, Some("run-1".to_string()));
    }

    #[tokio::test]
    async fn test_build_dag_multi_level() {
        let storage = MemoryStorage::new();

        // Level 0: root
        let root = create_test_run_record(
            "run-1", "root", None, "user_request", 100, "completed"
        );
        storage.save_run_record(&root).await.unwrap();

        // Level 1: child1
        let child1 = create_test_run_record(
            "run-2", "child1", Some("run-1".to_string()), "task_decomposition", 50, "completed"
        );
        storage.save_run_record(&child1).await.unwrap();
        let event1 = create_test_spawn_event(
            "run-1", "run-2", Some("run-1".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event1).await.unwrap();

        // Level 1: child2
        let child2 = create_test_run_record(
            "run-3", "child2", Some("run-1".to_string()), "tool_call", 30, "failed"
        );
        storage.save_run_record(&child2).await.unwrap();
        let event2 = create_test_spawn_event(
            "run-1", "run-3", Some("run-1".to_string()), "tool_call"
        );
        storage.save_spawn_event(&event2).await.unwrap();

        // Level 2: grandchild of child1
        let grandchild = create_test_run_record(
            "run-4", "grandchild", Some("run-2".to_string()), "task_decomposition", 20, "completed"
        );
        storage.save_run_record(&grandchild).await.unwrap();
        let event3 = create_test_spawn_event(
            "run-2", "run-4", Some("run-2".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event3).await.unwrap();

        let dag = build_dag(&storage, "run-1").await.unwrap();

        assert_eq!(dag.nodes.len(), 4);
        assert_eq!(dag.edges.len(), 3);
        assert_eq!(dag.roots.len(), 1);

        // Check descendants
        let root_descendants = dag.get_descendants("run-1");
        assert_eq!(root_descendants.len(), 3);
        assert!(root_descendants.contains(&"run-2".to_string()));
        assert!(root_descendants.contains(&"run-3".to_string()));
        assert!(root_descendants.contains(&"run-4".to_string()));

        let child1_descendants = dag.get_descendants("run-2");
        assert_eq!(child1_descendants.len(), 1);
        assert_eq!(child1_descendants[0], "run-4");
    }

    #[tokio::test]
    async fn test_render_tree() {
        let storage = MemoryStorage::new();

        let root = create_test_run_record(
            "run-1", "root-agent", None, "user_request", 100, "completed"
        );
        storage.save_run_record(&root).await.unwrap();

        let child = create_test_run_record(
            "run-2", "child-agent", Some("run-1".to_string()), "task_decomposition", 50, "failed"
        );
        storage.save_run_record(&child).await.unwrap();

        let event = create_test_spawn_event(
            "run-1", "run-2", Some("run-1".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event).await.unwrap();

        let dag = build_dag(&storage, "run-1").await.unwrap();
        let tree_output = dag.render_tree();

        assert!(tree_output.contains("root-agent"));
        assert!(tree_output.contains("child-agent"));
        assert!(tree_output.contains("✓")); // completed
        assert!(tree_output.contains("✗")); // failed
        assert!(tree_output.contains("100 tokens"));
        assert!(tree_output.contains("50 tokens"));
    }

    #[tokio::test]
    async fn test_render_dot() {
        let storage = MemoryStorage::new();

        let root = create_test_run_record(
            "run-1", "root-agent", None, "user_request", 100, "completed"
        );
        storage.save_run_record(&root).await.unwrap();

        let child = create_test_run_record(
            "run-2", "child-agent", Some("run-1".to_string()), "task_decomposition", 50, "completed"
        );
        storage.save_run_record(&child).await.unwrap();

        let event = create_test_spawn_event(
            "run-1", "run-2", Some("run-1".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event).await.unwrap();

        let dag = build_dag(&storage, "run-1").await.unwrap();
        let dot_output = dag.render_dot();

        assert!(dot_output.contains("digraph lineage_run_1"));
        assert!(dot_output.contains("root-agent"));
        assert!(dot_output.contains("child-agent"));
        assert!(dot_output.contains("task_decomposition"));
        assert!(dot_output.contains("->"));
        assert!(dot_output.contains("fillcolor"));
    }

    #[tokio::test]
    async fn test_render_json() {
        let storage = MemoryStorage::new();

        let root = create_test_run_record(
            "run-1", "root-agent", None, "user_request", 100, "completed"
        );
        storage.save_run_record(&root).await.unwrap();

        let dag = build_dag(&storage, "run-1").await.unwrap();
        let json_output = dag.render_json().unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&json_output).unwrap();
        assert_eq!(parsed["run_id"], "run-1");
        assert!(parsed["nodes"].is_object());
        assert!(parsed["edges"].is_array());
        assert!(parsed["roots"].is_array());
    }

    #[tokio::test]
    async fn test_impact_stats() {
        let storage = MemoryStorage::new();

        // Run 1: root-agent spawns child1 and child2
        let root1 = create_test_run_record(
            "run-1", "root-agent", None, "user_request", 100, "completed"
        );
        storage.save_run_record(&root1).await.unwrap();

        let child1 = create_test_run_record(
            "run-2", "child-agent", Some("run-1".to_string()), "task_decomposition", 50, "completed"
        );
        storage.save_run_record(&child1).await.unwrap();
        let event1 = create_test_spawn_event(
            "run-1", "run-2", Some("run-1".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event1).await.unwrap();

        let child2 = create_test_run_record(
            "run-3", "child-agent", Some("run-1".to_string()), "tool_call", 30, "failed"
        );
        storage.save_run_record(&child2).await.unwrap();
        let event2 = create_test_spawn_event(
            "run-1", "run-3", Some("run-1".to_string()), "tool_call"
        );
        storage.save_spawn_event(&event2).await.unwrap();

        // Run 2: another root-agent with different children
        let root2 = create_test_run_record(
            "run-4", "root-agent", None, "user_request", 200, "completed"
        );
        storage.save_run_record(&root2).await.unwrap();

        let child3 = create_test_run_record(
            "run-5", "child-agent", Some("run-4".to_string()), "task_decomposition", 40, "completed"
        );
        storage.save_run_record(&child3).await.unwrap();
        let event3 = create_test_spawn_event(
            "run-4", "run-5", Some("run-4".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event3).await.unwrap();

        let stats = impact_stats(&storage, "root-agent").await.unwrap();

        // root-agent appears in 2 runs
        // Run 1: root (100 tokens) + 2 children (50+30) = 180 tokens, 2 subtasks, 1 failed
        // Run 2: root (200 tokens) + 1 child (40) = 240 tokens, 1 subtask, 0 failed
        // Total: 3 subtasks, 420 tokens, 1/3 failure rate
        assert_eq!(stats.subtask_count, 3);
        assert_eq!(stats.total_tokens, 420);
        assert!((stats.failure_rate - 1.0/3.0).abs() < 0.001);
        assert_eq!(stats.failed_count, 1);
        assert_eq!(stats.success_count, 3); // root1, child1, root2, child3 = 4 completed, but we count root-agent nodes too
    }

    #[tokio::test]
    async fn test_get_ancestors() {
        let storage = MemoryStorage::new();

        let root = create_test_run_record(
            "run-1", "root", None, "user_request", 100, "completed"
        );
        storage.save_run_record(&root).await.unwrap();

        let child = create_test_run_record(
            "run-2", "child", Some("run-1".to_string()), "task_decomposition", 50, "completed"
        );
        storage.save_run_record(&child).await.unwrap();
        let event1 = create_test_spawn_event(
            "run-1", "run-2", Some("run-1".to_string()), "task_decomposition"
        );
        storage.save_spawn_event(&event1).await.unwrap();

        let grandchild = create_test_run_record(
            "run-3", "grandchild", Some("run-2".to_string()), "tool_call", 20, "completed"
        );
        storage.save_run_record(&grandchild).await.unwrap();
        let event2 = create_test_spawn_event(
            "run-2", "run-3", Some("run-2".to_string()), "tool_call"
        );
        storage.save_spawn_event(&event2).await.unwrap();

        let dag = build_dag(&storage, "run-1").await.unwrap();

        let ancestors = dag.get_ancestors("run-3");
        assert_eq!(ancestors.len(), 2);
        assert_eq!(ancestors[0], "run-2");
        assert_eq!(ancestors[1], "run-1");
    }
}
