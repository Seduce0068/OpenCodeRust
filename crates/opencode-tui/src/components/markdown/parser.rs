#[derive(Clone, Debug, PartialEq)]
pub enum MarkdownBlock {
    Heading {
        level: u8,
        content: String,
    },
    Paragraph {
        content: String,
        inline: Vec<InlineElement>,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    List {
        items: Vec<ListItem>,
        ordered: bool,
    },
    BlockQuote {
        content: String,
    },
    ThematicBreak,
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum InlineElement {
    Plain(String),
    Bold(String),
    Italic(String),
    BoldItalic(String),
    Code(String),
    Link { text: String, url: String },
    Strikethrough(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListItem {
    pub content: String,
    pub inline: Vec<InlineElement>,
    pub children: Vec<ListItem>,
}
