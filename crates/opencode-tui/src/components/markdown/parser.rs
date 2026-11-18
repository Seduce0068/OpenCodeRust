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

#[cfg(test)]
mod tests {
    use super::*;

    // --- MarkdownBlock: 各バリアントのデータ保持 ---
    // 部分型の PartialEq 有無に依存しないよう、matches! とフィールド比較で検証する。

    #[test]
    fn heading_block_retains_level_and_content() {
        let block = MarkdownBlock::Heading {
            level: 3,
            content: "見出し".to_string(),
        };
        assert!(matches!(
            &block,
            MarkdownBlock::Heading { level, content }
                if *level == 3 && content.as_str() == "見出し"
        ));
    }

    #[test]
    fn paragraph_block_retains_content_and_inline_elements() {
        let block = MarkdownBlock::Paragraph {
            content: "hello world".to_string(),
            inline: vec![
                InlineElement::Bold("hello".to_string()),
                InlineElement::Plain(" world".to_string()),
            ],
        };
        match &block {
            MarkdownBlock::Paragraph { content, inline } => {
                assert_eq!(content.as_str(), "hello world");
                assert_eq!(inline.len(), 2);
                assert!(matches!(&inline[0], InlineElement::Bold(s) if s.as_str() == "hello"));
                assert!(matches!(&inline[1], InlineElement::Plain(s) if s.as_str() == " world"));
            }
            _ => panic!("Paragraph バリアントが保持されませんでした"),
        }
    }

    #[test]
    fn code_block_retains_language_and_code() {
        let with_lang = MarkdownBlock::CodeBlock {
            language: Some("rust".to_string()),
            code: "fn main() {}".to_string(),
        };
        assert!(matches!(
            &with_lang,
            MarkdownBlock::CodeBlock { language: Some(lang), code }
                if lang.as_str() == "rust" && code.as_str() == "fn main() {}"
        ));

        let without_lang = MarkdownBlock::CodeBlock {
            language: None,
            code: "plain".to_string(),
        };
        assert!(matches!(
            &without_lang,
            MarkdownBlock::CodeBlock { language: None, code } if code.as_str() == "plain"
        ));
    }

    #[test]
    fn list_block_retains_items_and_ordered_flag() {
        let block = MarkdownBlock::List {
            items: vec![
                ListItem {
                    content: "one".to_string(),
                    inline: vec![],
                    children: vec![],
                },
                ListItem {
                    content: "two".to_string(),
                    inline: vec![],
                    children: vec![],
                },
            ],
            ordered: true,
        };
        match &block {
            MarkdownBlock::List { items, ordered } => {
                assert!(*ordered);
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].content.as_str(), "one");
                assert_eq!(items[1].content.as_str(), "two");
            }
            _ => panic!("List バリアントが保持されませんでした"),
        }

        let unordered = MarkdownBlock::List {
            items: vec![],
            ordered: false,
        };
        assert!(matches!(
            &unordered,
            MarkdownBlock::List { items, ordered } if !*ordered && items.is_empty()
        ));
    }

    #[test]
    fn block_quote_and_thematic_break_retain_shape() {
        let quote = MarkdownBlock::BlockQuote {
            content: "引用".to_string(),
        };
        assert!(matches!(
            &quote,
            MarkdownBlock::BlockQuote { content } if content.as_str() == "引用"
        ));

        let thematic_break = MarkdownBlock::ThematicBreak;
        assert!(matches!(thematic_break, MarkdownBlock::ThematicBreak));
    }

    #[test]
    fn table_block_retains_headers_and_rows() {
        let block = MarkdownBlock::Table {
            headers: vec!["A".to_string(), "B".to_string()],
            rows: vec![
                vec!["1".to_string(), "2".to_string()],
                vec!["3".to_string(), "4".to_string()],
            ],
        };
        match &block {
            MarkdownBlock::Table { headers, rows } => {
                assert_eq!(headers.len(), 2);
                assert_eq!(headers[0].as_str(), "A");
                assert_eq!(headers[1].as_str(), "B");
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0][0].as_str(), "1");
                assert_eq!(rows[0][1].as_str(), "2");
                assert_eq!(rows[1][0].as_str(), "3");
                assert_eq!(rows[1][1].as_str(), "4");
            }
            _ => panic!("Table バリアントが保持されませんでした"),
        }
    }

    #[test]
    fn markdown_block_clone_preserves_data() {
        let original = MarkdownBlock::Heading {
            level: 2,
            content: "clone".to_string(),
        };
        let cloned = original.clone();
        assert!(matches!(
            &cloned,
            MarkdownBlock::Heading { level, content }
                if *level == 2 && content.as_str() == "clone"
        ));
        // 元の値も変更されていないことを確認する。
        assert!(matches!(
            &original,
            MarkdownBlock::Heading { level, content }
                if *level == 2 && content.as_str() == "clone"
        ));
    }

    // --- InlineElement ---

    #[test]
    fn inline_elements_retain_their_payloads() {
        assert!(matches!(
            InlineElement::Plain("text".to_string()),
            InlineElement::Plain(s) if s == "text"
        ));
        assert!(matches!(
            InlineElement::Bold("b".to_string()),
            InlineElement::Bold(s) if s == "b"
        ));
        assert!(matches!(
            InlineElement::Italic("i".to_string()),
            InlineElement::Italic(s) if s == "i"
        ));
        assert!(matches!(
            InlineElement::BoldItalic("bi".to_string()),
            InlineElement::BoldItalic(s) if s == "bi"
        ));
        assert!(matches!(
            InlineElement::Code("c".to_string()),
            InlineElement::Code(s) if s == "c"
        ));
        assert!(matches!(
            InlineElement::Link {
                text: "t".to_string(),
                url: "u".to_string(),
            },
            InlineElement::Link { text, url } if text == "t" && url == "u"
        ));
        assert!(matches!(
            InlineElement::Strikethrough("s".to_string()),
            InlineElement::Strikethrough(s) if s == "s"
        ));
    }

    // --- ListItem ---

    #[test]
    fn list_item_retains_content_inline_and_nested_children() {
        let item = ListItem {
            content: "parent".to_string(),
            inline: vec![InlineElement::Code("x".to_string())],
            children: vec![ListItem {
                content: "child".to_string(),
                inline: vec![InlineElement::Italic("y".to_string())],
                children: vec![],
            }],
        };

        assert_eq!(item.content.as_str(), "parent");
        assert_eq!(item.inline.len(), 1);
        assert!(matches!(&item.inline[0], InlineElement::Code(s) if s.as_str() == "x"));
        assert_eq!(item.children.len(), 1);
        assert_eq!(item.children[0].content.as_str(), "child");
        assert!(matches!(
            &item.children[0].inline[0],
            InlineElement::Italic(s) if s.as_str() == "y"
        ));
        assert!(item.children[0].children.is_empty());
    }
}
