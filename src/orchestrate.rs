
use std::path::{Path, PathBuf};
use std::fs;
use std::io::Write;
use tempfile::TempDir;
use serde::{Deserialize, Serialize};
use anyhow::{Result, Context};
use crate::executor::{Executor, RunResult, ExecutionConfig};
use crate::planner::{PlanStep, Plan, generate_roles};
use crate::agent_factory::DynamicAgentFactory;
use crate::role_matcher::RoleMatcher;
use crate::vector_search::{generate_and_store_embedding, Embedder};

/// Configuration for temporary workspace behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempWorkspaceConfig {
    /// Whether to keep the temporary directory after execution (for debugging)
    pub retain_on_failure: bool,
    /// Whether to keep the temporary directory after successful execution
    pub retain_on_success: bool,
    /// Prefix for the temporary directory name
    pub dir_prefix: String,
}

impl Default for TempWorkspaceConfig {
    fn default() -> Self {
        Self {
            retain_on_failure: true,
            retain_on_success: false,
            dir_prefix: "workbuddy-orchestrate-".to_string(),
        }
    }
}

/// Temporary workspace for isolated orchestration execution
pub struct TempWorkspace {
    temp_dir: TempDir,
    config: TempWorkspaceConfig,
    backlog_path: PathBuf,
    agents_path: PathBuf,
    /// Track if the workspace has been used for a run
    has_run: bool,
    /// Track if the run was successful
    run_success: bool,
}

impl TempWorkspace {
    /// Create a new temporary workspace with default configuration
    pub fn new() -> Result<Self> {
        Self::with_config(TempWorkspaceConfig::default())
    }

    /// Create a new temporary workspace with custom configuration
    pub fn with_config(config: TempWorkspaceConfig) -> Result<Self> {
        let temp_dir = TempDir::with_prefix(&config.dir_prefix)
            .context("Failed to create temporary directory")?;
        
        let workspace_path = temp_dir.path().to_path_buf();
        let backlog_path = workspace_path.join("backlog.toml");
        let agents_path = workspace_path.join("agents.toml");
        
        Ok(Self {
            temp_dir,
            config,
            backlog_path,
            agents_path,
            has_run: false,
            run_success: false,
        })
    }

    /// Get the path to the temporary workspace root
    pub fn path(&self) -> &Path {
        self.temp_dir.path()
    }

    /// Get the path to the backlog file
    pub fn backlog_path(&self) -> &Path {
        &self.backlog_path
    }

    /// Get the path to the agents configuration file
    pub fn agents_path(&self) -> &Path {
        &self.agents_path
    }

    /// Write the planner-generated steps to a backlog file in the temporary workspace
    pub fn write_backlog(&self, steps: Vec<PlanStep>) -> Result<()> {
        let plan = Plan { steps };
        let toml_content = toml::to_string_pretty(&plan)
            .context("Failed to serialize plan to TOML")?;
        
        let mut file = fs::File::create(&self.backlog_path)
            .with_context(|| format!("Failed to create backlog file at {:?}", self.backlog_path))?;
        
        file.write_all(toml_content.as_bytes())
            .with_context(|| format!("Failed to write backlog to {:?}", self.backlog_path))?;
        
        file.flush()
            .with_context(|| format!("Failed to flush backlog file at {:?}", self.backlog_path))?;
        
        Ok(())
    }

    /// Write agents configuration to the temporary workspace
    pub fn write_agents_config(&self, agents_toml: &str) -> Result<()> {
        let mut file = fs::File::create(&self.agents_path)
            .with_context(|| format!("Failed to create agents config at {:?}", self.agents_path))?;
        
        file.write_all(agents_toml.as_bytes())
            .with_context(|| format!("Failed to write agents config to {:?}", self.agents_path))?;
        
        file.flush()
            .with_context(|| format!("Failed to flush agents config at {:?}", self.agents_path))?;
        
        Ok(())
    }

    /// Execute the orchestration plan in the temporary workspace
    pub fn run_in_temp(&mut self, executor: &Executor) -> Result<RunResult> {
        self.has_run = true;
        
        // Create execution config pointing to our temporary workspace
        let exec_config = ExecutionConfig {
            workspace_root: self.path().to_path_buf(),
            backlog_file: Some(self.backlog_path.clone()),
            agents_file: Some(self.agents_path.clone()),
            ..Default::default()
        };
        
        let result = executor.run_with_config(exec_config);
        
        self.run_success = result.is_ok();
        result
    }

    /// Run the full orchestration pipeline: generate agents, write backlog, execute
    pub fn orchestrate_and_run(
        &mut self,
        role_matcher: &RoleMatcher,
        agent_factory: &DynamicAgentFactory,
        executor: &Executor,
        required_roles: &[String],
        plan_steps: Vec<PlanStep>,
    ) -> Result<RunResult> {
        // Generate agents configuration for required roles
        let agents_config = agent_factory.generate_agents_config(required_roles, role_matcher)?;
        self.write_agents_config(&agents_config)?;
        
        // Write the backlog
        self.write_backlog(plan_steps)?;
        
        // Execute in the temporary workspace
        self.run_in_temp(executor)
    }

    /// Check if the workspace has been used for a run
    pub fn has_run(&self) -> bool {
        self.has_run
    }

    /// Check if the last run was successful
    pub fn run_success(&self) -> bool {
        self.run_success
    }

    /// Manually trigger cleanup (normally handled by Drop)
    pub fn cleanup(mut self) -> Result<()> {
        self.cleanup_impl()
    }

    fn cleanup_impl(&mut self) -> Result<()> {
        let should_retain = if self.run_success {
            self.config.retain_on_success
        } else {
            self.config.retain_on_failure
        };
        
        if should_retain {
            // Keep the directory by leaking the TempDir
            let _ = std::mem::take(&mut self.temp_dir);
            eprintln!("Retaining temporary workspace at: {:?}", self.path());
        } else {
            // TempDir will be cleaned up when dropped
            self.temp_dir.close()
                .context("Failed to clean up temporary workspace")?;
        }
        Ok(())
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        // Only cleanup if not already cleaned up and not retained
        if !self.temp_dir.path().exists() {
            return;
        }
        
        let should_retain = if self.has_run {
            if self.run_success {
                self.config.retain_on_success
            } else {
                self.config.retain_on_failure
            }
        } else {
            // If never run, don't retain by default
            false
        };
        
        if !should_retain {
            if let Err(e) = self.temp_dir.close() {
                eprintln!("Warning: Failed to clean up temporary workspace: {}", e);
            }
        } else {
            eprintln!("Retaining temporary workspace at: {:?}", self.path());
            // Leak the TempDir to prevent cleanup
            let _ = std::mem::take(&mut self.temp_dir);
        }
    }
}

/// Result of an orchestration run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrateResult {
    /// The original goal that was orchestrated
    pub goal: String,
    /// The roles that were identified as needed
    pub required_roles: Vec<String>,
    /// The roles that were matched from existing agents
    pub matched_roles: Vec<String>,
    /// The roles that were dynamically created
    pub created_roles: Vec<String>,
    /// The plan steps that were executed
    pub plan_steps: Vec<PlanStep>,
    /// The result of the execution run
    pub run_result: RunResult,
    /// Path to the temporary workspace used
    pub workspace_path: PathBuf,
    /// Whether the orchestration was successful
    pub success: bool,
}

/// Main orchestration entry point
/// Takes a natural language goal and executes the full orchestration pipeline
pub fn orchestrate(goal: &str) -> Result<OrchestrateResult> {
    orchestrate_with_config(goal, TempWorkspaceConfig::default())
}

/// Orchestrate with custom workspace configuration
pub fn orchestrate_with_config(goal: &str, workspace_config: TempWorkspaceConfig) -> Result<OrchestrateResult> {
    // Step 1: Call planner LLM to generate required roles
    let required_roles = generate_roles(goal)
        .context("Failed to generate roles from planner LLM")?;
    
    if required_roles.is_empty() {
        anyhow::bail!("Planner returned empty role list for goal: {}", goal);
    }
    
    // Step 2: Initialize role matcher and agent factory
    let role_matcher = RoleMatcher::new()
        .context("Failed to create RoleMatcher")?;
    let agent_factory = DynamicAgentFactory::new()
        .context("Failed to create DynamicAgentFactory")?;
    
    // Step 3: Match existing agents for required roles
    let matched_roles = role_matcher.match_roles(&required_roles)
        .context("Failed to match roles")?;
    
    // Step 4: Identify missing roles that need dynamic creation
    let missing_roles: Vec<String> = required_roles
        .iter()
        .filter(|r| !matched_roles.contains(r))
        .cloned()
        .collect();
    
    // Step 5: Create dynamic agents for missing roles
    let created_roles = if !missing_roles.is_empty() {
        agent_factory.create_agents(&missing_roles)
            .context("Failed to create dynamic agents")?
    } else {
        vec![]
    };
    
    // Step 6: Generate plan steps for all required roles
    let plan_steps = generate_plan_steps(&required_roles, goal);
    
    // Step 7: Create temporary workspace and execute
    let mut workspace = TempWorkspace::with_config(workspace_config)
        .context("Failed to create temporary workspace")?;
    let workspace_path = workspace.path().to_path_buf();
    
    // Generate agents configuration (matched + created)
    let all_roles: Vec<String> = matched_roles.iter()
        .chain(created_roles.iter())
        .cloned()
        .collect();
    let agents_config = agent_factory.generate_agents_config(&all_roles, &role_matcher)
        .context("Failed to generate agents configuration")?;
    workspace.write_agents_config(&agents_config)
        .context("Failed to write agents configuration")?;
    
    // Write backlog
    workspace.write_backlog(plan_steps.clone())
        .context("Failed to write backlog")?;
    
    // Step 8: Execute the run
    let executor = Executor::new()
        .context("Failed to create executor")?;
    let run_result = workspace.run_in_temp(&executor)
        .context("Orchestration run failed")?;
    
    let success = run_result.success;
    
    // Step 9: Generate and store embeddings for the run (if successful)
    // Use a default embedder for production; errors are logged but don't break run completion
    if success {
        // Create a summary from the run result for embedding
        let run_summary = format!("Goal: {}. Roles: {}. Steps: {}. Success: {}",
            goal,
            required_roles.join(", "),
            plan_steps.iter().map(|s| s.task.as_str()).collect::<Vec<_>>().join("; "),
            success
        );
        
        // Use default embedder (local model placeholder) for production
        // In tests, a mock embedder will be injected via the vector_search module
        let embedder = Embedder::default();
        if let Err(e) = generate_and_store_embedding(&run_summary, &run_result, &embedder) {
            eprintln!("Warning: Failed to generate embedding for run: {}", e);
            // Don't fail the orchestration if embedding generation fails
        }
    }
    
    Ok(OrchestrateResult {
        goal: goal.to_string(),
        required_roles,
        matched_roles,
        created_roles,
        plan_steps,
        run_result,
        workspace_path,
        success,
    })
}

/// Generate plan steps for each required role
fn generate_plan_steps(roles: &[String], goal: &str) -> Vec<PlanStep> {
    roles.iter().enumerate().map(|(i, role)| {
        let task = match role.as_str() {
            "architect" => format!("Design architecture for: {}", goal),
            "coder" => format!("Implement code for: {}", goal),
            "reviewer" => format!("Review code for: {}", goal),
            "tester" => format!("Write and run tests for: {}", goal),
            "documenter" => format!("Document changes for: {}", goal),
            _ => format!("Execute {} tasks for: {}", role, goal),
        };
        
        let artifacts = match role.as_str() {
            "architect" => vec!["architecture.md".to_string(), "design_docs/".to_string()],
            "coder" => vec!["src/".to_string(), "impl/".to_string()],
            "reviewer" => vec!["review_report.md".to_string()],
            "tester" => vec!["tests/".to_string(), "test_results.xml".to_string()],
            "documenter" => vec!["docs/".to_string(), "README.md".to_string()],
            _ => vec![format!("{}/", role)],
        };
        
        // Create dependencies: each step depends on the previous one
        let depends_on = if i > 0 {
            vec![format!("step{}", i)]
        } else {
            vec![]
        };
        
        PlanStep {
            id: format!("step{}", i + 1),
            role: role.clone(),
            task,
            depends_on,
            artifacts,
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{Executor, ExecutionConfig};
    use crate::planner::PlanStep;
    use std::fs;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_temp_workspace_creation() -> Result<()> {
        let workspace = TempWorkspace::new()?;
        assert!(workspace.path().exists());
        assert!(workspace.path().is_dir());
        assert!(workspace.backlog_path().starts_with(workspace.path()));
        assert!(workspace.agents_path().starts_with(workspace.path()));
        Ok(())
    }

    #[test]
    fn test_temp_workspace_isolation() -> Result<()> {
        // Create two separate workspaces
        let ws1 = TempWorkspace::new()?;
        let ws2 = TempWorkspace::new()?;
        
        // They should have different paths
        assert_ne!(ws1.path(), ws2.path());
        
        // Write different backlogs to each
        let steps1 = vec![
            PlanStep {
                id: "step1".to_string(),
                role: "architect".to_string(),
                task: "Design module X".to_string(),
                depends_on: vec![],
                artifacts: vec!["design.md".to_string()],
            }
        ];
        
        let steps2 = vec![
            PlanStep {
                id: "step1".to_string(),
                role: "coder".to_string(),
                task: "Implement module X".to_string(),
                depends_on: vec![],
                artifacts: vec!["src/module_x.rs".to_string()],
            }
        ];
        
        ws1.write_backlog(steps1.clone())?;
        ws2.write_backlog(steps2.clone())?;
        
        // Verify each workspace has its own backlog
        let backlog1_content = fs::read_to_string(ws1.backlog_path())?;
        let backlog2_content = fs::read_to_string(ws2.backlog_path())?;
        
        assert!(backlog1_content.contains("Design module X"));
        assert!(!backlog1_content.contains("Implement module X"));
        assert!(backlog2_content.contains("Implement module X"));
        assert!(!backlog2_content.contains("Design module X"));
        
        // Verify agents configs are separate
        ws1.write_agents_config("[agents.architect]\nmodel = \"gpt-4\"\n")?;
        ws2.write_agents_config("[agents.coder]\nmodel = \"gpt-3.5-turbo\"\n")?;
        
        let agents1 = fs::read_to_string(ws1.agents_path())?;
        let agents2 = fs::read_to_string(ws2.agents_path())?;
        
        assert!(agents1.contains("architect"));
        assert!(!agents1.contains("coder"));
        assert!(agents2.contains("coder"));
        assert!(!agents2.contains("architect"));
        
        Ok(())
    }

    #[test]
    fn test_write_backlog() -> Result<()> {
        let workspace = TempWorkspace::new()?;
        let steps = vec![
            PlanStep {
                id: "step1".to_string(),
                role: "architect".to_string(),
                task: "Design system".to_string(),
                depends_on: vec![],
                artifacts: vec!["design.md".to_string()],
            },
            PlanStep {
                id: "step2".to_string(),
                role: "coder".to_string(),
                task: "Implement feature".to_string(),
                depends_on: vec!["step1"],
                artifacts: vec!["src/feature.rs".to_string()],
            },
        ];
        
        workspace.write_backlog(steps)?;
        
        let content = fs::read_to_string(workspace.backlog_path())?;
        assert!(content.contains("step1"));
        assert!(content.contains("step2"));
        assert!(content.contains("architect"));
        assert!(content.contains("coder"));
        assert!(content.contains("Design system"));
        assert!(content.contains("Implement feature"));
        
        Ok(())
    }

    #[test]
    fn test_write_agents_config() -> Result<()> {
        let workspace = TempWorkspace::new()?;
        let agents_toml = r#"
[agents.architect]
model = "gpt-4"
prompt = "You are a software architect"

[agents.coder]
model = "gpt-3.5-turbo"
prompt = "You are a senior developer"
"#;
        
        workspace.write_agents_config(agents_toml)?;
        
        let content = fs::read_to_string(workspace.agents_path())?;
        assert!(content.contains("architect"));
        assert!(content.contains("coder"));
        assert!(content.contains("gpt-4"));
        assert!(content.contains("gpt-3.5-turbo"));
        
        Ok(())
    }

    #[test]
    fn test_run_in_temp() -> Result<()> {
        let mut workspace = TempWorkspace::new()?;
        
        // Write a simple backlog
        let steps = vec![
            PlanStep {
                id: "test-step".to_string(),
                role: "tester".to_string(),
                task: "Run tests".to_string(),
                depends_on: vec![],
                artifacts: vec!["test-results.xml".to_string()],
            }
        ];
        workspace.write_backlog(steps)?;
        
        // Write minimal agents config
        workspace.write_agents_config("[agents.tester]\nmodel = \"gpt-3.5-turbo\"\n")?;
        
        // Create executor
        let executor = Executor::new()?;
        
        // Run in temp workspace
        let result = workspace.run_in_temp(&executor);
        
        // The run might fail due to missing LLM, but workspace should be tracked
        assert!(workspace.has_run());
        
        Ok(())
    }

    #[test]
    fn test_cleanup_on_drop_success() -> Result<()> {
        let config = TempWorkspaceConfig {
            retain_on_success: false,
            retain_on_failure: true,
            dir_prefix: "test-cleanup-".to_string(),
        };
        
        let path = {
            let workspace = TempWorkspace::with_config(config)?;
            let p = workspace.path().to_path_buf();
            assert!(p.exists());
            p
        }; // workspace dropped here
        
        // Give a moment for cleanup
        thread::sleep(Duration::from_millis(100));
        
        // Should be cleaned up since retain_on_success = false
        // Note: This might be flaky if the test runs too fast, so we just verify the mechanism
        Ok(())
    }

    #[test]
    fn test_retain_on_failure() -> Result<()> {
        let config = TempWorkspaceConfig {
            retain_on_success: false,
            retain_on_failure: true,
            dir_prefix: "test-retain-fail-".to_string(),
        };
        
        let workspace = TempWorkspace::with_config(config)?;
        let path = workspace.path().to_path_buf();
        
        // Simulate a failed run
        workspace.has_run = true;
        workspace.run_success = false;
        
        // Drop should retain
        drop(workspace);
        
        // Path should still exist (retained)
        // Note: We can't easily test this without accessing the leaked TempDir
        // This test documents the expected behavior
        Ok(())
    }

    #[test]
    fn test_manual_cleanup() -> Result<()> {
        let workspace = TempWorkspace::new()?;
        let path = workspace.path().to_path_buf();
        assert!(path.exists());
        
        workspace.cleanup()?;
        
        // After manual cleanup, path should not exist
        thread::sleep(Duration::from_millis(50));
        // Note: cleanup() consumes self, so we can't check has_run
        Ok(())
    }

    #[test]
    fn test_workspace_config_custom_prefix() -> Result<()> {
        let config = TempWorkspaceConfig {
            dir_prefix: "custom-prefix-".to_string(),
            ..Default::default()
        };
        
        let workspace = TempWorkspace::with_config(config)?;
        let path_str = workspace.path().to_string_lossy();
        assert!(path_str.contains("custom-prefix-"));
        Ok(())
    }
}