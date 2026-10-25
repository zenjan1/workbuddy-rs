#[cfg(feature = "tui")]
use std::{io::stdout, sync::Arc, time::Duration};

#[cfg(feature = "tui")]
use anyhow::Result;
#[cfg(feature = "tui")]
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
#[cfg(feature = "tui")]
use ratatui::{
    backend::{CrosstermBackend, TestBackend},
    layout::{Constraint, Direction, Layout},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
#[cfg(feature = "tui")]
use tokio::sync::mpsc;
#[cfg(feature = "tui")]
use tokio::task::JoinHandle;
#[cfg(feature = "tui")]
use crate::executor::task_runner::{run_task, TaskEvent, TaskConfig};
#[cfg(feature = "tui")]
use crate::event_bus::{EventBus, EventBusSubscriber};

/// A single backlog entry used by the TUI list. Construct these from
/// workspace records in `main.rs` (e.g. `StepRecord`) before calling `run_tui`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogItem {
    pub id: usize,
    pub title: String,
    pub status: String,
    pub priority: i64,
}

impl BacklogItem {
    pub fn new(id: usize, title: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            status: status.into(),
            priority: 0,
        }
    }

    pub fn with_priority(
        id: usize,
        title: impl Into<String>,
        status: impl Into<String>,
        priority: i64,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            status: status.into(),
            priority,
        }
    }
}

/// Task alias for external callers that already use the name `Task`.
pub type Task = BacklogItem;

/// Aggregated state handed to the TUI renderer/loop. Kept separate from
/// terminal/runtime concerns so tests can exercise the pure renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogState {
    pub tasks: Vec<BacklogItem>,
}

impl BacklogState {
    pub fn new(tasks: Vec<BacklogItem>) -> Self {
        Self { tasks }
    }
}

/// Pure mapping from backlog data to list row strings.
///
/// The UI reuses this function so a test on this function is also a test on
/// the basic list content.
pub fn map_backlog_to_rows(items: &[Task]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            let existing_slugs: std::collections::HashSet<&str> = std::collections::HashSet::new();
            let slug = crate::task_id::TaskId::from_title(&item.title, item.id, &existing_slugs).to_string();
            format!("[{}] {:>3}. {} [{}]", slug, item.id, item.title, item.status)
        })
        .collect()
}

/// Pure function requested by the TUI skeleton step: formats backlog tasks
/// as terminal rows without touching global state.
pub fn render_backlog_tasks(tasks: &[Task]) -> Vec<String> {
    map_backlog_to_rows(tasks)
}

/// Load backlog items from the workspace backlog file.
///
/// Returns an empty list when the file does not exist.
pub fn load_backlog(workspace_dir: &std::path::Path) -> anyhow::Result<Vec<Task>> {
    let backlog_path = workspace_dir.join("backlog.json");
    if !backlog_path.exists() {
        return Ok(Vec::new());
    }

    let data = std::fs::read_to_string(&backlog_path)?;
    let parsed: serde_json::Value = serde_json::from_str(&data)?;
    let mut items = Vec::new();
    if let Some(tasks) = parsed.get("tasks").and_then(|v| v.as_array()) {
        for (idx, task) in tasks.iter().enumerate() {
            let title = task
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("(无标题)")
                .to_string();
            let priority = task
                .get("priority")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let skip = task
                .get("skip")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let status = if skip {
                "skip".to_string()
            } else {
                format!("优先级 {}", priority)
            };
            items.push(Task::with_priority(idx + 1, title, status, priority));
        }
    }
    Ok(items)
}

fn status_rank(status: &str) -> u8 {
    match status {
        "待办" | "pending" => 0,
        "运行中" | "running" => 1,
        "失败" | "failed" => 2,
        "完成" | "done" => 3,
        _ => 4,
    }
}

/// Pure sorting: status rank ascending, priority descending, id ascending.
///
/// This is the canonical backlog ordering used by both the live TUI and
/// snapshot rendering, so it can be unit tested without terminal access.
pub fn sort_backlog(items: Vec<BacklogItem>) -> Vec<BacklogItem> {
    let mut items = items;
    items.sort_by(|a, b| {
        status_rank(&a.status)
            .cmp(&status_rank(&b.status))
            .then_with(|| b.priority.cmp(&a.priority))
            .then_with(|| a.id.cmp(&b.id))
    });
    items
}

/// Pure refresh: replace current state with freshly loaded backlog data.
///
/// The new items are sorted with [`sort_backlog`] so callers don't need to
/// duplicate ordering logic when polling a file or API.
pub fn refresh_backlog(_current: &BacklogState, fresh: Vec<BacklogItem>) -> BacklogState {
    BacklogState::new(sort_backlog(fresh))
}

#[cfg(feature = "tui")]
/// Pure rendering function: sorts the backlog list by status and priority,
/// draws it with ratatui's TestBackend, and returns the resulting static
/// snapshot as a string. It includes the "Backlog" title and task count in
/// the output.
pub fn render_backlog(state: &BacklogState) -> String {
    let tasks = sort_backlog(state.tasks.clone());

    let rows = if tasks.is_empty() {
        vec!["No backlog tasks".to_string()]
    } else {
        render_backlog_tasks(&tasks)
    };

    let list_items: Vec<ListItem> = rows.iter().map(|row| ListItem::new(row.clone())).collect();
    let title = format!(" Backlog ({} tasks) ", tasks.len());
    let list = List::new(list_items).block(Block::default().borders(Borders::ALL).title(title));

    let height = (rows.len() as u16 + 4).max(10);
    let backend = ratatui::backend::TestBackend::new(80, height);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    terminal
        .draw(|frame| {
            frame.render_widget(list, frame.area());
        })
        .expect("failed to draw backlog snapshot");

    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output.trim().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    MoveUp,
    MoveDown,
    Quit,
    None,
}

#[cfg(feature = "tui")]
/// Pure key-state handling.
///
/// Returns the next selected index and an intent describing what the key
/// means. Up/down are clamped to valid list boundaries; `q` returns `Quit`
/// without changing the selection.
pub fn handle_key(key: KeyCode, selected: usize, item_count: usize) -> (usize, KeyAction) {
    match key {
        KeyCode::Char('q') => (selected, KeyAction::Quit),
        KeyCode::Up => (selected.saturating_sub(1), KeyAction::MoveUp),
        KeyCode::Down => {
            let next = selected.saturating_add(1);
            if next < item_count {
                (next, KeyAction::MoveDown)
            } else {
                (selected, KeyAction::MoveDown)
            }
        }
        _ => (selected, KeyAction::None),
    }
}

/// Application state for the TUI.
#[cfg(feature = "tui")]
#[derive(Debug)]
pub struct App {
    pub backlog: Vec<BacklogItem>,
    pub list_state: ListState,
    pub show_details: bool,
    pub should_quit: bool,
    /// Log buffer for the bottom panel
    pub log_buffer: Vec<String>,
    /// Scroll offset for log panel
    pub log_scroll: usize,
    /// Whether a task is currently running
    pub is_running: bool,
    /// Handle to the running task
    pub task_handle: Option<JoinHandle<()>>,
    /// Receiver for task events (log lines, completion)
    pub task_event_rx: Option<mpsc::UnboundedReceiver<TaskEvent>>,
    /// Event bus subscriber for external events
    pub event_bus_sub: Option<EventBusSubscriber>,
}

#[cfg(feature = "tui")]
impl App {
    pub fn new(tasks: Vec<BacklogItem>) -> Self {
        let mut list_state = ListState::default();
        if !tasks.is_empty() {
            list_state.select(Some(0));
        }
        Self {
            backlog: tasks,
            list_state,
            show_details: false,
            should_quit: false,
            log_buffer: Vec::new(),
            log_scroll: 0,
            is_running: false,
            task_handle: None,
            task_event_rx: None,
            event_bus_sub: None,
        }
    }

    /// Spawn a task (run or dev) and set up event handling
    pub async fn spawn_task(&mut self, command: &str) -> Result<()> {
        if self.is_running {
            return Ok(());
        }

        let config = TaskConfig {
            command: command.to_string(),
            args: vec![],
            env: std::collections::HashMap::new(),
            cwd: None,
        };

        let (handle, rx) = run_task(config).await?;
        self.task_handle = Some(handle);
        self.task_event_rx = Some(rx);
        self.is_running = true;
        self.log_buffer.clear();
        self.log_buffer.push(format!("[INFO] Starting task: {}", command));
        self.log_scroll = self.log_buffer.len().saturating_sub(1);
        Ok(())
    }

    /// Poll task events and update log buffer
    pub fn poll_task_events(&mut self) {
        if let Some(rx) = &mut self.task_event_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    TaskEvent::LogLine(line) => {
                        self.log_buffer.push(line);
                        // Auto-scroll to bottom
                        self.log_scroll = self.log_buffer.len().saturating_sub(1);
                    }
                    TaskEvent::Completed(exit_code) => {
                        self.log_buffer.push(format!("[INFO] Task completed with exit code: {}", exit_code));
                        self.is_running = false;
                        self.task_handle = None;
                        self.task_event_rx = None;
                    }
                    TaskEvent::Error(err) => {
                        self.log_buffer.push(format!("[ERROR] {}", err));
                        self.is_running = false;
                        self.task_handle = None;
                        self.task_event_rx = None;
                    }
                }
            }
        }
    }

    /// Handle key events
    pub fn handle_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char('q') => {
                if !self.is_running {
                    self.should_quit = true;
                }
            }
            KeyCode::Char('r') => {
                if !self.is_running {
                    let _ = self.spawn_task("run");
                }
            }
            KeyCode::Char('d') => {
                if !self.is_running {
                    let _ = self.spawn_task("dev");
                }
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.backlog.is_empty() {
                    let next = self.list_state.selected().map_or(0, |s| s + 1);
                    if next < self.backlog.len() {
                        self.list_state.select(Some(next));
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.backlog.is_empty() {
                    let prev = self.list_state.selected().map_or(0, |s| s.saturating_sub(1));
                    self.list_state.select(Some(prev));
                }
            }
            KeyCode::Enter => {
                self.show_details = !self.show_details;
            }
            KeyCode::PageUp => {
                self.log_scroll = self.log_scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                let max_scroll = self.log_buffer.len().saturating_sub(1);
                self.log_scroll = (self.log_scroll + 10).min(max_scroll);
            }
            KeyCode::Home => {
                self.log_scroll = 0;
            }
            KeyCode::End => {
                self.log_scroll = self.log_buffer.len().saturating_sub(1);
            }
            _ => {}
        }
    }
}

/// Run the blocking TUI with three panels: left shows backlog list, right shows task details, bottom shows logs.
/// Supports j/k for navigation, Enter to toggle details, r/d to run tasks, q to quit, PgUp/PgDn for log scrolling.
#[cfg(feature = "tui")]
pub fn run_tui(tasks: Vec<Task>) -> Result<()> {
    let tasks = sort_backlog(tasks);
    let mut app = App::new(tasks);
    
    // Subscribe to event bus for external events
    let event_bus = EventBus::global();
    app.event_bus_sub = Some(event_bus.subscribe());

    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create a tokio runtime for async task spawning
    let rt = tokio::runtime::Runtime::new()?;

    loop {
        // Poll task events
        app.poll_task_events();
        
        // Poll event bus
        if let Some(sub) = &mut app.event_bus_sub {
            while let Ok(event) = sub.try_recv() {
                app.log_buffer.push(format!("[EVENT] {:?}", event));
                app.log_scroll = app.log_buffer.len().saturating_sub(1);
            }
        }

        terminal.draw(|frame| draw_ui(frame, &mut app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key.code);
                if app.should_quit {
                    break;
                }
            }
        }
    }

    // Cleanup: abort running task if any
    if let Some(handle) = app.task_handle.take() {
        handle.abort();
    }
    
    // Wait a bit for cleanup
    rt.block_on(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
    });

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

#[cfg(feature = "tui")]
fn draw_ui(frame: &mut Frame, app: &mut App) {
    let items = &app.backlog;
    let selected = app.list_state.selected().unwrap_or(0);
    
    let rows = if items.is_empty() {
        vec!["No backlog tasks".to_string()]
    } else {
        render_backlog_tasks(items)
    };

    let list_items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let cursor = if idx == selected && !items.is_empty() {
                ">> "
            } else {
                "   "
            };
            ListItem::new(format!("{}{}", cursor, row))
        })
        .collect();

    let status_hint = if app.is_running {
        " (RUNNING - r/d disabled, q to quit after stop)"
    } else {
        ""
    };
    let list = List::new(list_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Backlog ({} tasks, j/k: move, Enter: details, r: run, d: dev, q: quit{}) ", items.len(), status_hint)),
    );

    // Create three-panel layout: left (backlog), right (details), bottom (logs)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(12),
        ])
        .split(frame.area());

    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[0]);

    frame.render_stateful_widget(list, top_chunks[0], &mut app.list_state);

    // Right panel: task details
    if app.show_details && !items.is_empty() && selected < items.len() {
        let task = &items[selected];
        let details = format!(
            "Title: {}\nStatus: {}\nPriority: {}\nID: {}\n\nDescription:\n{}\n\nDependencies:\n{}",
            task.title,
            task.status,
            task.priority,
            task.id,
            "(No description available)",
            "(No dependencies)"
        );
        let paragraph = Paragraph::new(details)
            .block(Block::default().borders(Borders::ALL).title(" Task Details "));
        frame.render_widget(paragraph, top_chunks[1]);
    } else {
        let placeholder = Paragraph::new("Press Enter to view task details")
            .block(Block::default().borders(Borders::ALL).title(" Task Details "));
        frame.render_widget(placeholder, top_chunks[1]);
    }

    // Bottom panel: log output
    let log_lines: Vec<ListItem> = app.log_buffer
        .iter()
        .skip(app.log_scroll)
        .take(chunks[1].height as usize)
        .map(|line| ListItem::new(line.clone()))
        .collect();

    let log_title = if app.is_running {
        " Logs (LIVE - PgUp/PgDn: scroll, Home/End: jump) "
    } else {
        " Logs (PgUp/PgDn: scroll, Home/End: jump) "
    };
    let log_list = List::new(log_lines)
        .block(Block::default().borders(Borders::ALL).title(log_title));
    frame.render_widget(log_list, chunks[1]);
}

#[cfg(feature = "tui")]
pub fn render(frame: &mut Frame, items: &[BacklogItem]) {
    if items.is_empty() {
        let list = List::new(vec![ListItem::new("No backlog tasks")])
            .block(Block::default().borders(Borders::ALL).title(" Backlog "));
        frame.render_widget(list, frame.area());
        return;
    }

    let rows: Vec<ListItem> = items
        .iter()
        .map(|item| {
            ListItem::new(format!("{} [{}] (priority: {})", item.title, item.status, item.priority))
        })
        .collect();

    let list = List::new(rows).block(Block::default().borders(Borders::ALL).title(" Backlog "));
    frame.render_widget(list, frame.area());
}

#[cfg(all(test, feature = "tui"))]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    fn sample_items() -> Vec<BacklogItem> {
        vec![
            BacklogItem::new(1, "Design TUI module", "pending"),
            BacklogItem::new(2, "Render backlog list", "pending"),
            BacklogItem::new(3, "Handle q to quit", "done"),
        ]
    }

    #[test]
    fn app_new_initializes_state() {
        let items = sample_items();
        let app = App::new(items.clone());
        assert_eq!(app.backlog.len(), 3);
        assert_eq!(app.list_state.selected(), Some(0));
        assert!(!app.show_details);
        assert!(!app.should_quit);
        assert!(app.log_buffer.is_empty());
        assert_eq!(app.log_scroll, 0);
        assert!(!app.is_running);
        assert!(app.task_handle.is_none());
        assert!(app.task_event_rx.is_none());
    }

    #[test]
    fn app_handle_key_quit_when_not_running() {
        let mut app = App::new(sample_items());
        app.handle_key(KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn app_handle_key_quit_blocked_when_running() {
        let mut app = App::new(sample_items());
        app.is_running = true;
        app.handle_key(KeyCode::Char('q'));
        assert!(!app.should_quit);
    }

    #[test]
    fn app_handle_key_navigation() {
        let mut app = App::new(sample_items());
        assert_eq!(app.list_state.selected(), Some(0));
        
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.list_state.selected(), Some(1));
        
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.list_state.selected(), Some(2));
        
        app.handle_key(KeyCode::Char('j'));
        assert_eq!(app.list_state.selected(), Some(2));
        
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.list_state.selected(), Some(1));
        
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.list_state.selected(), Some(0));
        
        app.handle_key(KeyCode::Char('k'));
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn app_handle_key_enter_toggles_details() {
        let mut app = App::new(sample_items());
        assert!(!app.show_details);
        app.handle_key(KeyCode::Char('\n'));
        assert!(app.show_details);
        app.handle_key(KeyCode::Char('\n'));
        assert!(!app.show_details);
    }

    #[test]
    fn app_handle_key_r_d_when_not_running() {
        let mut app = App::new(sample_items());
        // These would spawn tasks in real usage, but we just verify they don't panic
        app.handle_key(KeyCode::Char('r'));
        app.handle_key(KeyCode::Char('d'));
        // is_running would be set to true by spawn_task in real usage
    }

    #[test]
    fn app_handle_key_log_scrolling() {
        let mut app = App::new(sample_items());
        app.log_buffer = (0..100).map(|i| format!("Line {}", i)).collect();
        
        app.handle_key(KeyCode::End);
        assert_eq!(app.log_scroll, 99);
        
        app.handle_key(KeyCode::Home);
        assert_eq!(app.log_scroll, 0);
        
        app.handle_key(KeyCode::PageDown);
        assert_eq!(app.log_scroll, 10);
        
        app.handle_key(KeyCode::PageUp);
        assert_eq!(app.log_scroll, 0);
    }

    #[test]
    fn log_panel_renders_correctly() {
        let mut app = App::new(sample_items());
        app.log_buffer = vec![
            "[INFO] Starting task: run".to_string(),
            "[INFO] Compiling...".to_string(),
            "[INFO] Done".to_string(),
        ];
        
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        terminal.draw(|frame| draw_ui(frame, &mut app)).expect("draw failed");
        
        let buffer = terminal.backend().buffer();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        
        assert!(output.contains("[INFO] Starting task: run"));
        assert!(output.contains("[INFO] Compiling..."));
        assert!(output.contains("[INFO] Done"));
        assert!(output.contains("Logs"));
    }

    #[test]
    fn log_panel_auto_scrolls_on_new_lines() {
        let mut app = App::new(sample_items());
        app.log_buffer = vec!["Line 1".to_string(), "Line 2".to_string()];
        app.log_scroll = 0;
        
        // Simulate adding a new line (like poll_task_events would do)
        app.log_buffer.push("Line 3".to_string());
        app.log_scroll = app.log_buffer.len().saturating_sub(1);
        
        assert_eq!(app.log_scroll, 2);
    }

    #[test]
    fn maps_backlog_to_rows_content_and_count() {
        let rows = map_backlog_to_rows(&sample_items());

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], "[design-tui-module]   1. Design TUI module [pending]");
        assert_eq!(rows[1], "[render-backlog-list]   2. Render backlog list [pending]");
        assert_eq!(rows[2], "[handle-q-to-quit]   3. Handle q to quit [done]");
    }

    #[test]
    fn up_down_are_bounded() {
        let len = 3;

        assert_eq!(handle_key(KeyCode::Up, 0, len), (0, KeyAction::MoveUp));
        assert_eq!(handle_key(KeyCode::Up, 2, len), (1, KeyAction::MoveUp));
        assert_eq!(handle_key(KeyCode::Down, 0, len), (1, KeyAction::MoveDown));
        assert_eq!(handle_key(KeyCode::Down, 2, len), (2, KeyAction::MoveDown));
        assert_eq!(handle_key(KeyCode::Up, 0, 0), (0, KeyAction::MoveUp));
        assert_eq!(handle_key(KeyCode::Down, 0, 0), (0, KeyAction::MoveDown));
    }

    #[test]
    fn q_returns_quit_intent() {
        assert_eq!(handle_key(KeyCode::Char('q'), 2, 5), (2, KeyAction::Quit));
        assert_eq!(handle_key(KeyCode::Char('a'), 1, 5), (1, KeyAction::None));
    }

    #[test]
    fn maps_backlog_to_rows_empty_list() {
        let items: Vec<BacklogItem> = vec![];
        let rows = map_backlog_to_rows(&items);
        assert!(rows.is_empty());
    }

    #[test]
    fn maps_backlog_to_rows_preserves_special_characters() {
        let items = vec![
            BacklogItem::new(1, "Fix \"quotes\" & <angles>", "done"),
            BacklogItem::new(2, "Unicode: 任务 ✓", "pending"),
            BacklogItem::new(3, "Line\nbreak", "todo"),
        ];
        let rows = map_backlog_to_rows(&items);
        assert_eq!(
            rows,
            vec![
                "[fix-quotes-angles]   1. Fix \"quotes\" & <angles> [done]".to_string(),
                "[unicode-任务]   2. Unicode: 任务 ✓ [pending]".to_string(),
                "[line-break]   3. Line\nbreak [todo]".to_string(),
            ]
        );
    }

    #[test]
    fn render_backlog_contains_title_and_count() {
        let state = BacklogState::new(sample_items());
        let output = render_backlog(&state);

        assert!(output.contains("Backlog"), "output should contain title: {output}");
        assert!(output.contains("3"), "output should contain task count: {output}");
        assert!(output.find("[design-tui-module]   1. Design TUI module [pending]").is_some(), "output should contain first pending row: {output}");
        assert!(output.find("[render-backlog-list]   2. Render backlog list [pending]").is_some(), "output should contain second pending row: {output}");
    }

    #[test]
    fn render_backlog_empty_shows_placeholder() {
        let state = BacklogState::new(vec![]);
        let output = render_backlog(&state);

        assert!(output.contains("Backlog"), "output should contain title: {output}");
        assert!(output.contains("0"), "output should contain zero task count: {output}");
        assert!(output.contains("No backlog tasks"), "output should contain empty placeholder: {output}");
    }

    #[test]
    fn render_backlog_sorts_by_status_and_priority() {
        let tasks = vec![
            BacklogItem::with_priority(3, "Failed step", "失败", 5),
            BacklogItem::with_priority(1, "Write plan", "待办", 7),
            BacklogItem::with_priority(2, "Execute plan", "运行中", 9),
            BacklogItem::with_priority(4, "Second pending", "待办", 2),
        ];
        let state = BacklogState::new(tasks);
        let output = render_backlog(&state);

        let idx_pending_first = output.find("[write-plan]").expect("first pending row");
        let idx_pending_second = output.find("[second-pending]").expect("second pending row");
        let idx_running = output.find("[execute-plan]").expect("running row");
        let idx_failed = output.find("[failed-step]").expect("failed row");

        assert!(idx_pending_first < idx_pending_second, "higher priority pending task should come first: {output}");
        assert!(idx_pending_second < idx_running, "pending tasks before running: {output}");
        assert!(idx_running < idx_failed, "running tasks before failed: {output}");
    }

    #[test]
    fn render_backlog_tasks_formats_expected_rows() {
        let rows = render_backlog_tasks(&sample_items());

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], "[design-tui-module]   1. Design TUI module [pending]");
        assert_eq!(rows[1], "[render-backlog-list]   2. Render backlog list [pending]");
        assert_eq!(rows[2], "[handle-q-to-quit]   3. Handle q to quit [done]");
    }

    #[test]
    fn render_static_does_not_panic() {
        let items = sample_items();
        let backend = ratatui::backend::TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        terminal.draw(|frame| render(frame, &items)).expect("draw failed");
    }

    #[test]
    fn sort_backlog_sorts_by_status_priority_and_id() {
        let tasks = vec![
            BacklogItem::with_priority(4, "Second pending", "待办", 2),
            BacklogItem::with_priority(3, "Failed step", "失败", 5),
            BacklogItem::with_priority(1, "Write plan", "待办", 7),
            BacklogItem::with_priority(2, "Execute plan", "运行中", 9),
        ];

        let sorted = sort_backlog(tasks);
        let ids: Vec<_> = sorted.iter().map(|task| task.id).collect();

        assert_eq!(ids, vec![1, 4, 2, 3]);
    }

    #[test]
    fn refresh_backlog_replaces_old_state_and_sorts() {
        let current = BacklogState::new(sample_items());
        let fresh = vec![
            BacklogItem::with_priority(11, "New pending", "待办", 1),
            BacklogItem::with_priority(10, "New failed", "失败", 9),
        ];

        let refreshed = refresh_backlog(&current, fresh);

        assert_eq!(refreshed.tasks.len(), 2);
        assert_eq!(refreshed.tasks[0].title, "New pending");
        assert_eq!(refreshed.tasks[1].title, "New failed");
        assert_eq!(current.tasks.len(), 3, "refresh must not mutate current state");
    }
}

#[cfg(feature = "tui")]
pub fn run(workspace_dir: &std::path::Path) -> Result<()> {
    let items = load_backlog(workspace_dir)?;
    run_tui(items)
}

/// A run entry used by the TUI run list. Construct these from
/// workspace run records with their annotations before calling `run_runs_tui`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunItem {
    pub id: String,
    pub status: String,
    pub started_at: String,
    pub tags: Vec<(String, String)>, // (key, value) pairs
    pub has_note: bool,
}

impl RunItem {
    pub fn new(
        id: impl Into<String>,
        status: impl Into<String>,
        started_at: impl Into<String>,
        tags: Vec<(String, String)>,
        has_note: bool,
    ) -> Self {
        Self {
            id: id.into(),
            status: status.into(),
            started_at: started_at.into(),
            tags,
            has_note,
        }
    }
}

/// Aggregated state for the runs TUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsState {
    pub runs: Vec<RunItem>,
}

impl RunsState {
    pub fn new(runs: Vec<RunItem>) -> Self {
        Self { runs }
    }
}

/// Format tags as a compact string: key1=val1,key2=val2
pub fn format_tags(tags: &[(String, String)]) -> String {
    if tags.is_empty() {
        String::new()
    } else {
        tags.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Pure mapping from runs data to list row strings.
/// Shows: ID, status, started_at, tags, note indicator
pub fn map_runs_to_rows(items: &[RunItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            let tags_str = format_tags(&item.tags);
            let note_indicator = if item.has_note { " 📝" } else { "" };
            let tags_display = if tags_str.is_empty() {
                "(no tags)".to_string()
            } else {
                tags_str
            };
            format!(
                "{} | {:<8} | {} | {}{}",
                item.id, item.status, item.started_at, tags_display, note_indicator
            )
        })
        .collect()
}

/// Pure function: formats runs as terminal rows without touching global state.
pub fn render_runs_tasks(runs: &[RunItem]) -> Vec<String> {
    map_runs_to_rows(runs)
}

/// Load runs with annotations from the workspace.
/// This uses local annotation loading logic (mirroring store.rs).
pub fn load_runs_with_annotations(
    workspace_dir: &std::path::Path,
) -> anyhow::Result<Vec<RunItem>> {
    let runs_dir = workspace_dir.join("runs");
    let run_ids = list_run_ids_local(&runs_dir)?;
    let mut items = Vec::new();
    
    for run_id in run_ids {
        let annotations = load_annotations_local(&runs_dir, &run_id)?;
        let tags: Vec<(String, String)> = annotations
            .tags
            .into_iter()
            .collect();
        let has_note = annotations.note.is_some();
        
        // Try to load run metadata for status and started_at
        let (status, started_at) = load_run_metadata(&runs_dir, &run_id)
            .unwrap_or(("unknown".to_string(), "unknown".to_string()));
        
        items.push(RunItem::new(
            run_id,
            status,
            started_at,
            tags,
            has_note,
        ));
    }
    
    Ok(items)
}

/// Local copy of Annotations struct from store.rs
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
struct LocalAnnotations {
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub tags: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Local copy of sidecar_path from store.rs
fn sidecar_path_local(runs_dir: &std::path::Path, run_id: &str) -> std::path::PathBuf {
    runs_dir.join(format!("{}.meta.json", run_id))
}

/// Local copy of list_run_ids from store.rs
fn list_run_ids_local(runs_dir: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let mut run_ids = Vec::new();
    
    if !runs_dir.exists() {
        return Ok(run_ids);
    }

    for entry in std::fs::read_dir(runs_dir)? {
        let entry = entry?;
        let path = entry.path();
        
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

/// Local copy of load_annotations from store.rs
fn load_annotations_local(runs_dir: &std::path::Path, run_id: &str) -> anyhow::Result<LocalAnnotations> {
    let path = sidecar_path_local(runs_dir, run_id);
    
    if !path.exists() {
        return Ok(LocalAnnotations::default());
    }

    let content = std::fs::read_to_string(&path)?;
    if content.trim().is_empty() {
        return Ok(LocalAnnotations::default());
    }

    // Try to parse as Annotations directly (new format)
    if let Ok(annotations) = serde_json::from_str::<LocalAnnotations>(&content) {
        return Ok(annotations);
    }

    // Backward compatibility: try to parse as generic JSON and extract tags/note
    let value: serde_json::Value = serde_json::from_str(&content)?;
    let mut annotations = LocalAnnotations::default();
    
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

/// Load run metadata (status, started_at) from run.json
fn load_run_metadata(runs_dir: &std::path::Path, run_id: &str) -> anyhow::Result<(String, String)> {
    let run_path = runs_dir.join(run_id).join("run.json");
    if !run_path.exists() {
        return Err(anyhow::anyhow!("run.json not found"));
    }
    
    let content = std::fs::read_to_string(&run_path)?;
    let value: serde_json::Value = serde_json::from_str(&content)?;
    
    let status = value.get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let started_at = value.get("started_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    
    Ok((status, started_at))
}

#[cfg(feature = "tui")]
/// Pure rendering function for runs list: draws it with ratatui's TestBackend,
/// and returns the resulting static snapshot as a string.
pub fn render_runs(state: &RunsState) -> String {
    let rows = if state.runs.is_empty() {
        vec!["No runs found".to_string()]
    } else {
        render_runs_tasks(&state.runs)
    };

    let list_items: Vec<ListItem> = rows.iter().map(|row| ListItem::new(row.clone())).collect();
    let title = format!(" Runs ({} runs) ", state.runs.len());
    let list = List::new(list_items).block(Block::default().borders(Borders::ALL).title(title));

    let height = (rows.len() as u16 + 4).max(10);
    let backend = ratatui::backend::TestBackend::new(120, height);
    let mut terminal = Terminal::new(backend).expect("failed to create test terminal");
    terminal
        .draw(|frame| {
            frame.render_widget(list, frame.area());
        })
        .expect("failed to draw runs snapshot");

    let buffer = terminal.backend().buffer();
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            output.push_str(buffer[(x, y)].symbol());
        }
        output.push('\n');
    }
    output.trim().to_string()
}

#[cfg(feature = "tui")]
/// Run the blocking TUI for runs list with tags and notes display.
/// Supports j/k for navigation, q to quit.
pub fn run_runs_tui(runs: Vec<RunItem>) -> Result<()> {
    let items = runs;
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut selected = 0usize;

    loop {
        terminal.draw(|frame| draw_runs_ui(frame, &items, selected))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('j') | KeyCode::Down => {
                        if selected + 1 < items.len() {
                            selected += 1;
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if selected > 0 {
                            selected -= 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

#[cfg(feature = "tui")]
fn draw_runs_ui(frame: &mut Frame, items: &[RunItem], selected: usize) {
    let rows = if items.is_empty() {
        vec!["No runs found".to_string()]
    } else {
        render_runs_tasks(items)
    };

    let list_items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let cursor = if idx == selected && !items.is_empty() {
                ">> "
            } else {
                "   "
            };
            ListItem::new(format!("{}{}", cursor, row))
        })
        .collect();

    let list = List::new(list_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(
                " Runs ({} runs, j/k: move, q: quit) ",
                items.len()
            )),
    );

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected));
    }

    frame.render_stateful_widget(list, frame.area(), &mut state);
}

#[cfg(feature = "tui")]
/// Entry point for runs TUI from workspace directory.
pub fn run_runs(workspace_dir: &std::path::Path) -> Result<()> {
    let items = load_runs_with_annotations(workspace_dir)?;
    run_runs_tui(items)
}

#[cfg(all(test, feature = "tui"))]
mod runs_tests {
    use super::*;

    fn sample_runs() -> Vec<RunItem> {
        vec![
            RunItem::new(
                "run-001",
                "completed",
                "2024-01-15 10:30",
                vec![("env".to_string(), "prod".to_string()), ("team".to_string(), "backend".to_string())],
                true,
            ),
            RunItem::new(
                "run-002",
                "failed",
                "2024-01-15 11:00",
                vec![("env".to_string(), "staging".to_string())],
                false,
            ),
            RunItem::new(
                "run-003",
                "running",
                "2024-01-15 11:30",
                vec![],
                true,
            ),
        ]
    }

    #[test]
    fn format_tags_empty() {
        assert_eq!(format_tags(&[]), "");
    }

    #[test]
    fn format_tags_single() {
        assert_eq!(format_tags(&[("key".to_string(), "val".to_string())]), "key=val");
    }

    #[test]
    fn format_tags_multiple() {
        let tags = vec![
            ("key1".to_string(), "val1".to_string()),
            ("key2".to_string(), "val2".to_string()),
        ];
        assert_eq!(format_tags(&tags), "key1=val1,key2=val2");
    }

    #[test]
    fn maps_runs_to_rows_content_and_count() {
        let rows = map_runs_to_rows(&sample_runs());

        assert_eq!(rows.len(), 3);
        assert!(rows[0].contains("run-001"));
        assert!(rows[0].contains("completed"));
        assert!(rows[0].contains("env=prod,team=backend"));
        assert!(rows[0].contains("📝"));
        assert!(rows[1].contains("run-002"));
        assert!(rows[1].contains("failed"));
        assert!(rows[1].contains("env=staging"));
        assert!(!rows[1].contains("📝"));
        assert!(rows[2].contains("run-003"));
        assert!(rows[2].contains("running"));
        assert!(rows[2].contains("(no tags)"));
        assert!(rows[2].contains("📝"));
    }

    #[test]
    fn maps_runs_to_rows_empty_list() {
        let items: Vec<RunItem> = vec![];
        let rows = map_runs_to_rows(&items);
        assert!(rows.is_empty());
    }

    #[test]
    fn render_runs_contains_title_and_count() {
        let state = RunsState::new(sample_runs());
        let output = render_runs(&state);

        assert!(output.contains("Runs"), "output should contain title: {output}");
        assert!(output.contains("3"), "output should contain run count: {output}");
        assert!(output.contains("run-001"), "output should contain first run: {output}");
        assert!(output.contains("env=prod,team=backend"), "output should contain tags: {output}");
        assert!(output.contains("📝"), "output should contain note indicator: {output}");
    }

    #[test]
    fn render_runs_empty_shows_placeholder() {
        let state = RunsState::new(vec![]);
        let output = render_runs(&state);

        assert!(output.contains("Runs"), "output should contain title: {output}");
        assert!(output.contains("0"), "output should contain zero run count: {output}");
        assert!(output.contains("No runs found"), "output should contain empty placeholder: {output}");
    }

    #[test]
    fn render_runs_tasks_formats_expected_rows() {
        let rows = render_runs_tasks(&sample_runs());

        assert_eq!(rows.len(), 3);
        assert!(rows[0].contains("env=prod,team=backend"));
        assert!(rows[0].contains("📝"));
        assert!(rows[1].contains("env=staging"));
        assert!(!rows[1].contains("📝"));
        assert!(rows[2].contains("(no tags)"));
        assert!(rows[2].contains("📝"));
    }

    #[test]
    fn render_static_runs_does_not_panic() {
        let items = sample_runs();
        let backend = ratatui::backend::TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).expect("failed to create terminal");
        terminal
            .draw(|frame| {
                let rows = render_runs_tasks(&items);
                let list_items: Vec<ListItem> = rows.iter().map(|r| ListItem::new(r.clone())).collect();
                let list = List::new(list_items).block(Block::default().borders(Borders::ALL).title(" Runs "));
                frame.render_widget(list, frame.area());
            })
            .expect("draw failed");
    }
}
