use std::{io::stdout, time::Duration};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    widgets::{Block, Borders, List, ListItem, ListState},
    Frame, Terminal,
};

/// A single backlog entry used by the TUI list. Construct these from
/// workspace records in `main.rs` (e.g. `StepRecord`) before calling `run_tui`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogItem {
    pub id: usize,
    pub title: String,
    pub status: String,
}

impl BacklogItem {
    pub fn new(id: usize, title: impl Into<String>, status: impl Into<String>) -> Self {
        Self {
            id,
            title: title.into(),
            status: status.into(),
        }
    }
}

/// Pure mapping from backlog data to list row strings.
///
/// The UI reuses this function so a test on this function is also a test on
/// the basic list content.
pub fn map_backlog_to_rows(items: &[BacklogItem]) -> Vec<String> {
    items
        .iter()
        .map(|item| format!("{:>3}. {} [{}]", item.id, item.title, item.status))
        .collect()
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
pub fn run_tui(items: &[BacklogItem]) -> Result<()> {
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
        map_backlog_to_rows(items)
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
            .title(" Backlog (Up/Down: move, q: quit) "),
    );

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(selected));
    }
    frame.render_stateful_widget(list, frame.size(), &mut state);
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
        assert_eq!(rows[0], "  1. Design TUI module [pending]");
        assert_eq!(rows[1], "  2. Render backlog list [pending]");
        assert_eq!(rows[2], "  3. Handle q to quit [done]");
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
}
