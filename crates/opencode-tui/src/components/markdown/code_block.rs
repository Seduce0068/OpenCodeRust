use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

use super::syntax;

#[derive(Clone, Debug)]
pub struct CodeBlock {
    pub language: Option<String>,
    pub code: String,
}

impl CodeBlock {
    pub fn new(language: Option<String>, code: String) -> Self {
        Self { language, code }
    }

    pub fn to_lines(&self, theme: &CodeTheme) -> Vec<Vec<Span<'static>>> {
        if let Some(lines) = syntax::highlight_code(
            &self.code,
            self.language.as_deref().unwrap_or_default(),
            theme,
        ) {
            return lines;
        }

        let mut lines = Vec::new();

        for line in self.code.lines() {
            let highlighted = self.highlight_line(line, theme);
            lines.push(highlighted);
        }

        lines
    }

    fn highlight_line(&self, line: &str, theme: &CodeTheme) -> Vec<Span<'static>> {
        let language = self.language.as_deref().unwrap_or("");

        match language {
            "rust" => self.highlight_rust(line, theme),
            "python" | "py" => self.highlight_python(line, theme),
            "javascript" | "js" | "typescript" | "ts" => self.highlight_js(line, theme),
            "json" => self.highlight_json(line, theme),
            "bash" | "sh" | "shell" => self.highlight_bash(line, theme),
            _ => vec![Span::styled(
                line.to_string(),
                Style::default().fg(theme.text),
            )],
        }
    }

    fn highlight_rust(&self, line: &str, theme: &CodeTheme) -> Vec<Span<'static>> {
        let keywords = [
            "fn", "let", "mut", "const", "static", "pub", "mod", "use", "crate", "self", "Self",
            "super", "struct", "enum", "impl", "trait", "type", "where", "for", "loop", "while",
            "if", "else", "match", "return", "break", "continue", "async", "await", "move", "ref",
            "as", "in",
        ];

        highlight_with_keywords(line, theme, &keywords)
    }

    fn highlight_python(&self, line: &str, theme: &CodeTheme) -> Vec<Span<'static>> {
        let keywords = [
            "def", "class", "if", "elif", "else", "for", "while", "try", "except", "finally",
            "with", "as", "import", "from", "return", "yield", "raise", "pass", "break",
            "continue", "lambda", "and", "or", "not", "in", "is", "True", "False", "None", "async",
            "await",
        ];

        highlight_with_keywords(line, theme, &keywords)
    }

    fn highlight_js(&self, line: &str, theme: &CodeTheme) -> Vec<Span<'static>> {
        let keywords = [
            "function",
            "const",
            "let",
            "var",
            "if",
            "else",
            "for",
            "while",
            "return",
            "class",
            "extends",
            "import",
            "export",
            "from",
            "default",
            "async",
            "await",
            "try",
            "catch",
            "finally",
            "throw",
            "new",
            "this",
            "true",
            "false",
            "null",
            "undefined",
            "typeof",
            "instanceof",
        ];

        highlight_with_keywords(line, theme, &keywords)
    }

    fn highlight_json(&self, line: &str, theme: &CodeTheme) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let mut current = String::new();
        let mut in_string = false;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let ch = chars[i];

            if ch == '"' && (i == 0 || chars[i - 1] != '\\') {
                if !current.is_empty() && !in_string {
                    let trimmed = current.trim();
                    if trimmed == ":"
                        || trimmed == "{"
                        || trimmed == "}"
                        || trimmed == "["
                        || trimmed == "]"
                        || trimmed == ","
                    {
                        spans.push(Span::styled(
                            current.clone(),
                            Style::default().fg(theme.punctuation),
                        ));
                    } else if trimmed == "true" || trimmed == "false" || trimmed == "null" {
                        spans.push(Span::styled(
                            current.clone(),
                            Style::default().fg(theme.keyword),
                        ));
                    } else if trimmed.parse::<f64>().is_ok() {
                        spans.push(Span::styled(
                            current.clone(),
                            Style::default().fg(theme.number),
                        ));
                    } else {
                        spans.push(Span::styled(
                            current.clone(),
                            Style::default().fg(theme.text),
                        ));
                    }
                    current.clear();
                }

                in_string = !in_string;
                current.push(ch);
            } else {
                current.push(ch);
            }
            i += 1;
        }

        if !current.is_empty() {
            if in_string {
                spans.push(Span::styled(
                    current.clone(),
                    Style::default().fg(theme.string),
                ));
            } else {
                let trimmed = current.trim();
                if trimmed.parse::<f64>().is_ok() {
                    spans.push(Span::styled(
                        current.clone(),
                        Style::default().fg(theme.number),
                    ));
                } else if trimmed == "true" || trimmed == "false" || trimmed == "null" {
                    spans.push(Span::styled(
                        current.clone(),
                        Style::default().fg(theme.keyword),
                    ));
                } else {
                    spans.push(Span::styled(
                        current.clone(),
                        Style::default().fg(theme.text),
                    ));
                }
            }
        }

        if spans.is_empty() {
            vec![Span::styled(
                line.to_string(),
                Style::default().fg(theme.text),
            )]
        } else {
            spans
        }
    }

    fn highlight_bash(&self, line: &str, theme: &CodeTheme) -> Vec<Span<'static>> {
        let keywords = [
            "if", "then", "else", "elif", "fi", "for", "do", "done", "while", "until", "case",
            "esac", "function", "return", "exit", "export", "source", "echo", "printf", "read",
            "cd", "pwd", "ls", "mkdir", "rm", "cp", "mv", "cat", "grep", "sed", "awk", "find",
            "chmod", "chown", "sudo", "apt", "yum", "dnf", "pacman", "brew", "pip", "cargo",
        ];

        highlight_with_keywords(line, theme, &keywords)
    }
}

fn highlight_with_keywords(line: &str, theme: &CodeTheme, keywords: &[&str]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    let in_comment = false;
    let string_chars = ['"', '\''];

    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with('#') {
        return vec![Span::styled(
            line.to_string(),
            Style::default().fg(theme.comment),
        )];
    }

    for ch in line.chars() {
        if in_comment {
            current.push(ch);
            continue;
        }

        if string_chars.contains(&ch) {
            if !in_string {
                if !current.is_empty() {
                    add_keyword_span(&mut spans, &current, theme, keywords);
                    current.clear();
                }
                in_string = true;
                current.push(ch);
            } else {
                current.push(ch);
                spans.push(Span::styled(
                    current.clone(),
                    Style::default().fg(theme.string),
                ));
                current.clear();
                in_string = false;
            }
        } else if in_string {
            current.push(ch);
        } else if ch.is_whitespace() || is_punctuation(ch) {
            if !current.is_empty() {
                add_keyword_span(&mut spans, &current, theme, keywords);
                current.clear();
            }
            spans.push(Span::styled(
                ch.to_string(),
                Style::default().fg(theme.punctuation),
            ));
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        if in_string || in_comment {
            spans.push(Span::styled(
                current.clone(),
                Style::default().fg(if in_comment {
                    theme.comment
                } else {
                    theme.string
                }),
            ));
        } else {
            add_keyword_span(&mut spans, &current, theme, keywords);
        }
    }

    if spans.is_empty() {
        vec![Span::styled(
            line.to_string(),
            Style::default().fg(theme.text),
        )]
    } else {
        spans
    }
}

fn add_keyword_span(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    theme: &CodeTheme,
    keywords: &[&str],
) {
    let trimmed = text.trim();

    if keywords.contains(&trimmed) {
        spans.push(Span::styled(
            text.to_string(),
            Style::default()
                .fg(theme.keyword)
                .add_modifier(Modifier::BOLD),
        ));
    } else if trimmed.parse::<f64>().is_ok() {
        spans.push(Span::styled(
            text.to_string(),
            Style::default().fg(theme.number),
        ));
    } else {
        spans.push(Span::styled(
            text.to_string(),
            Style::default().fg(theme.text),
        ));
    }
}

fn is_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | ';'
            | ','
            | '.'
            | ':'
            | '='
            | '+'
            | '-'
            | '*'
            | '/'
            | '<'
            | '>'
            | '!'
            | '&'
            | '|'
    )
}

#[derive(Clone, Debug)]
pub struct CodeTheme {
    pub text: Color,
    pub keyword: Color,
    pub string: Color,
    pub number: Color,
    pub comment: Color,
    pub punctuation: Color,
    pub function: Color,
}

impl Default for CodeTheme {
    fn default() -> Self {
        Self {
            text: Color::Rgb(220, 220, 220),
            keyword: Color::Rgb(255, 121, 198),
            string: Color::Rgb(230, 219, 116),
            number: Color::Rgb(174, 129, 255),
            comment: Color::Rgb(117, 113, 94),
            punctuation: Color::Rgb(248, 248, 242),
            function: Color::Rgb(102, 217, 239),
        }
    }
}

impl CodeTheme {
    pub fn from_app_theme(theme: &crate::theme::Theme) -> Self {
        Self {
            text: theme.text,
            keyword: theme.primary,
            string: theme.success,
            number: theme.warning,
            comment: theme.text_muted,
            punctuation: theme.border,
            function: theme.info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_theme() -> CodeTheme {
        CodeTheme::default()
    }

    fn text_of(spans: &[Span<'static>]) -> String {
        spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn new_retains_language_and_code() {
        let block = CodeBlock::new(Some("rust".to_string()), "fn main() {}".to_string());

        assert_eq!(block.language.as_deref(), Some("rust"));
        assert_eq!(block.code.as_str(), "fn main() {}");
    }

    #[test]
    fn new_retains_none_language_and_code() {
        let block = CodeBlock::new(None, "plain text".to_string());

        assert!(block.language.is_none());
        assert_eq!(block.code.as_str(), "plain text");
    }

    #[test]
    fn to_lines_empty_code_returns_no_lines() {
        let theme = test_theme();
        let block = CodeBlock::new(None, String::new());

        assert!(block.to_lines(&theme).is_empty());
    }

    #[test]
    fn to_lines_splits_lines_for_unknown_language() {
        let theme = test_theme();
        let block = CodeBlock::new(
            Some("unknown_language_token_xyz".to_string()),
            "alpha\nbeta\ngamma".to_string(),
        );

        let lines = block.to_lines(&theme);
        assert_eq!(lines.len(), 3);
        for (line, expected) in lines.iter().zip(["alpha", "beta", "gamma"]) {
            assert_eq!(line.len(), 1);
            assert_eq!(line[0].content.as_ref(), expected);
            assert_eq!(line[0].style.fg, Some(theme.text));
        }
    }

    #[test]
    fn to_lines_unknown_language_keeps_trailing_and_blank_lines_behavior() {
        let theme = test_theme();
        let block = CodeBlock::new(None, "a\n\nb\n".to_string());

        let lines = block.to_lines(&theme);
        // str::lines(): "a\n\nb\n" -> ["a", "", "b"]
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0][0].content.as_ref(), "a");
        assert_eq!(lines[1][0].content.as_ref(), "");
        assert_eq!(lines[2][0].content.as_ref(), "b");
        for line in &lines {
            assert_eq!(line.len(), 1);
            assert_eq!(line[0].style.fg, Some(theme.text));
        }
    }

    #[test]
    fn to_lines_unknown_language_does_not_apply_keyword_highlight() {
        let theme = test_theme();
        let block = CodeBlock::new(
            Some("unknown_language_token_xyz".to_string()),
            "fn main() {}".to_string(),
        );

        let lines = block.to_lines(&theme);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(lines[0][0].content.as_ref(), "fn main() {}");
        assert_eq!(lines[0][0].style.fg, Some(theme.text));
        assert!(!lines[0][0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn to_lines_rust_uses_syntect_and_preserves_content() {
        let theme = test_theme();
        let source = "fn main() {\n    let x = 42;\n}\n";
        let block = CodeBlock::new(Some("rust".to_string()), source.to_string());

        let lines = block.to_lines(&theme);
        assert_eq!(lines.len(), 3);
        let expected: Vec<&str> = source.lines().collect();
        for (line, want) in lines.iter().zip(expected) {
            assert!(!line.is_empty());
            assert_eq!(text_of(line).as_str(), want);
        }
        assert!(lines.iter().any(|line| line.len() > 1));
    }

    #[test]
    fn to_lines_python_uses_syntect_and_preserves_content() {
        let theme = test_theme();
        let source = "def add(a, b):\n    return a + b\n";
        let block = CodeBlock::new(Some("python".to_string()), source.to_string());

        let lines = block.to_lines(&theme);
        assert_eq!(lines.len(), 2);
        let expected: Vec<&str> = source.lines().collect();
        for (line, want) in lines.iter().zip(expected) {
            assert!(!line.is_empty());
            assert_eq!(text_of(line).as_str(), want);
        }
        assert!(lines.iter().any(|line| line.len() > 1));
    }

    #[test]
    fn highlight_rust_keyword_uses_keyword_color_and_bold() {
        let theme = test_theme();
        let block = CodeBlock::new(Some("rust".to_string()), String::new());

        let spans = block.highlight_rust("fn", &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "fn");
        assert_eq!(spans[0].style.fg, Some(theme.keyword));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn highlight_python_keyword_uses_keyword_color_and_bold() {
        let theme = test_theme();
        let block = CodeBlock::new(Some("python".to_string()), String::new());

        let spans = block.highlight_python("def", &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "def");
        assert_eq!(spans[0].style.fg, Some(theme.keyword));
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn highlight_rust_comment_uses_comment_color() {
        let theme = test_theme();
        let block = CodeBlock::new(Some("rust".to_string()), String::new());

        let spans = block.highlight_rust("// note", &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "// note");
        assert_eq!(spans[0].style.fg, Some(theme.comment));
    }

    #[test]
    fn highlight_python_string_uses_string_color() {
        let theme = test_theme();
        let block = CodeBlock::new(Some("python".to_string()), String::new());

        let spans = block.highlight_python("\"hi\"", &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "\"hi\"");
        assert_eq!(spans[0].style.fg, Some(theme.string));
    }

    #[test]
    fn highlight_rust_number_uses_number_color() {
        let theme = test_theme();
        let block = CodeBlock::new(Some("rust".to_string()), String::new());

        let spans = block.highlight_rust("42", &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content.as_ref(), "42");
        assert_eq!(spans[0].style.fg, Some(theme.number));
    }
}
