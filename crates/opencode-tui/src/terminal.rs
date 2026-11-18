use std::io;

use crate::branding::{APP_NAME, APP_SHORT_NAME};

/// 端末 I/O を伴わずに既定のタイトル文字列を組み立てる。
fn format_base_title() -> String {
    format!("{} ({})", APP_NAME, APP_SHORT_NAME)
}

/// 端末 I/O を伴わずにセッション付きタイトル文字列を組み立てる。
fn format_session_title(session_title: &str) -> String {
    format!("{} ({}) - {}", APP_NAME, APP_SHORT_NAME, session_title)
}

pub fn set_title(title: &str) -> io::Result<()> {
    crossterm::execute!(io::stdout(), crossterm::terminal::SetTitle(title))
}

pub fn set_session_title(session_title: &str) -> io::Result<()> {
    set_title(&format_session_title(session_title))
}

pub fn reset_title() -> io::Result<()> {
    set_title(&format_base_title())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_title_has_app_name_and_short_name_in_parentheses() {
        let title = format_base_title();

        assert!(title.starts_with(APP_NAME));
        assert!(title.ends_with(&format!("({})", APP_SHORT_NAME)));
    }

    #[test]
    fn session_title_extends_base_with_separator_and_session() {
        let base = format_base_title();
        let title = format_session_title("session-42");

        assert!(title.starts_with(&base));
        assert_eq!(&title[base.len()..], " - session-42");
    }

    #[test]
    fn session_title_preserves_empty_session_after_separator() {
        let base = format_base_title();

        assert_eq!(format_session_title(""), format!("{} - ", base));
    }

    #[test]
    fn session_title_preserves_unicode_and_whitespace() {
        let base = format_base_title();
        let session = "  セッション 🚀  ";

        assert_eq!(
            format_session_title(session),
            format!("{} - {}", base, session)
        );
    }
}
