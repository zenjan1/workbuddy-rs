#[cfg(feature = "tui")]
mod tui_snapshot_test {
    use insta::assert_snapshot;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use xi::tui::{App, Tui};

    #[test]
    fn test_tui_initial_render() {
        // Create a test backend with a fixed size
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Create the app with some sample data
        let mut app = App::new();
        app.add_sample_tasks();

        // Run the TUI for one frame (initial render)
        let mut tui = Tui::new(terminal, app);
        tui.run_single_frame().unwrap();

        // Get the rendered frame
        let frame = tui.terminal().get_frame().clone();
        let buffer = frame.buffer();

        // Convert buffer to string representation for snapshot
        let rendered = buffer_to_string(buffer);
        assert_snapshot!("tui_initial_render", rendered);
    }

    #[test]
    fn test_tui_navigation_down() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new();
        app.add_sample_tasks();

        let mut tui = Tui::new(terminal, app);
        tui.run_single_frame().unwrap();

        // Simulate pressing 'j' to move down
        tui.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
        tui.run_single_frame().unwrap();

        let frame = tui.terminal().get_frame().clone();
        let buffer = frame.buffer();
        let rendered = buffer_to_string(buffer);
        assert_snapshot!("tui_navigation_down", rendered);
    }

    #[test]
    fn test_tui_navigation_up() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = App::new();
        app.add_sample_tasks();

        let mut tui = Tui::new(terminal, app);
        tui.run_single_frame().unwrap();

        // Move down first, then up
        tui.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
        tui.run_single_frame().unwrap();
        tui.handle_key_event(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('k'),
            crossterm::event::KeyModifiers::NONE,
        ));
        tui.run_single_frame().unwrap();

        let frame = tui.terminal().get_frame().clone();
        let buffer = frame.buffer();
        let rendered = buffer_to_string(buffer);
        assert_snapshot!("tui_navigation_up", rendered);
    }

    /// Helper to convert a ratatui buffer to a string for snapshot testing
    fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
        let mut output = String::new();
        for y in buffer.area.top()..buffer.area.bottom() {
            for x in buffer.area.left()..buffer.area.right() {
                let cell = buffer.get(x, y);
                let ch = cell.symbol();
                if ch == " " {
                    output.push('·'); // Use middle dot for spaces to make them visible
                } else {
                    output.push_str(ch);
                }
            }
            output.push('\n');
        }
        output
    }
}
