use std::{io::stdout, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Constraint,
    widgets::{Block, Borders, Cell, List, ListItem, ListState, Row, Table},
    Frame, Terminal,
};

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
pub fn load_backlog(workspace_dir: &std::path::Path) -> Result<Vec<Task>> {
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

/// Run the blocking TUI. Renders `items` as a selectable list, supports
/// Up/Down to move the cursor and `q` to quit.
pub fn run_tui(tasks: Vec<Task>) -> Result<()> {
    let tasks = sort_backlog(tasks);
    let items = &tasks;
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut selected = 0usize;

    loop {
        terminal.draw(|frame| draw_ui(frame, items, selected))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                let (next_selected, action) = handle_key(key.code, selected, items.len());
                selected = next_selected;
                if action == KeyAction::Quit {
                    break;
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn draw_ui(frame: &mut Frame, items: &[BacklogItem], selected: usize) {
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

    let list = List::new(list_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(" Backlog ({} tasks, Up/Down: move, q: quit) ", items.len())),
    );

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected));
    }
    frame.render_stateful_widget(list, frame.area(), &mut state);
}

pub fn render(frame: &mut Frame, items: &[BacklogItem]) {
    if items.is_empty() {
        let list = List::new(vec![ListItem::new("No backlog tasks")])
            .block(Block::default().borders(Borders::ALL).title(" Backlog "));
        frame.render_widget(list, frame.area());
        return;
    }

    let header = Row::new(vec![
        Cell::from("Title"),
        Cell::from("Status"),
        Cell::from("Priority"),
    ]);

    let rows: Vec<Row> = items
        .iter()
        .map(|item| {
            Row::new(vec![
                Cell::from(item.title.clone()),
                Cell::from(item.status.clone()),
                Cell::from(item.priority.to_string()),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(50),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(" Backlog "));

    frame.render_widget(table, frame.area());
}

#[cfg(test)]
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

pub fn run(workspace_dir: &std::path::Path) -> Result<()> {
    let items = load_backlog(workspace_dir)?;
    run_tui(items)
}