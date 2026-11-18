use ratatui::{layout::Rect, Frame};

pub struct Dialog {
    _title: String,
    open: bool,
}

impl Dialog {
    pub fn new(title: &str) -> Self {
        Self {
            _title: title.to_string(),
            open: false,
        }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn render(&self, _frame: &mut Frame, _area: Rect) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    // --- new / is_open ---

    #[test]
    fn new_starts_closed_and_keeps_title() {
        let dialog = Dialog::new("Greeting");

        assert!(!dialog.is_open());
        assert_eq!(dialog._title, "Greeting");
    }

    #[test]
    fn new_accepts_empty_title() {
        let dialog = Dialog::new("");

        assert!(!dialog.is_open());
        assert!(dialog._title.is_empty());
    }

    // --- open / close (require &mut self) ---

    #[test]
    fn open_marks_dialog_open() {
        let mut dialog = Dialog::new("Greeting");
        assert!(!dialog.is_open());

        dialog.open();

        assert!(dialog.is_open());
        assert_eq!(dialog._title, "Greeting");
    }

    #[test]
    fn close_clears_open_state() {
        let mut dialog = Dialog::new("Greeting");
        dialog.open();
        assert!(dialog.is_open());

        dialog.close();

        assert!(!dialog.is_open());
    }

    #[test]
    fn open_and_close_are_idempotent() {
        let mut dialog = Dialog::new("Greeting");

        dialog.close();
        dialog.close();
        assert!(!dialog.is_open());

        dialog.open();
        dialog.open();
        assert!(dialog.is_open());

        dialog.close();
        assert!(!dialog.is_open());
    }

    // --- render (no-op) ---

    #[test]
    fn render_is_noop_and_leaves_buffer_blank() {
        let mut dialog = Dialog::new("Greeting");
        dialog.open();

        let backend = TestBackend::new(8, 2);
        let mut terminal = Terminal::new(backend).expect("TestBackend should initialize");
        terminal
            .draw(|frame| {
                dialog.render(frame, Rect::new(0, 0, 8, 2));
            })
            .expect("draw should succeed");

        assert!(dialog.is_open());
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                assert_eq!(
                    buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "),
                    " "
                );
            }
        }
    }
}
