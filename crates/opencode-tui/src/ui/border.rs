pub struct BorderChars {
    pub top_left: char,
    pub top_right: char,
    pub bottom_left: char,
    pub bottom_right: char,
    pub horizontal: char,
    pub vertical: char,
}

impl Clone for BorderChars {
    fn clone(&self) -> Self {
        Self {
            top_left: self.top_left,
            top_right: self.top_right,
            bottom_left: self.bottom_left,
            bottom_right: self.bottom_right,
            horizontal: self.horizontal,
            vertical: self.vertical,
        }
    }
}

impl Default for BorderChars {
    fn default() -> Self {
        Self::rounded()
    }
}

impl BorderChars {
    pub fn rounded() -> Self {
        Self {
            top_left: '╭',
            top_right: '╮',
            bottom_left: '╰',
            bottom_right: '╯',
            horizontal: '─',
            vertical: '│',
        }
    }

    pub fn squared() -> Self {
        Self {
            top_left: '┌',
            top_right: '┐',
            bottom_left: '└',
            bottom_right: '┘',
            horizontal: '─',
            vertical: '│',
        }
    }

    pub fn thick() -> Self {
        Self {
            top_left: '┏',
            top_right: '┓',
            bottom_left: '┗',
            bottom_right: '┛',
            horizontal: '━',
            vertical: '┃',
        }
    }

    pub fn empty() -> Self {
        Self {
            top_left: ' ',
            top_right: ' ',
            bottom_left: ' ',
            bottom_right: ' ',
            horizontal: ' ',
            vertical: ' ',
        }
    }

    pub fn custom(vertical: char, bottom_left: char) -> Self {
        Self {
            top_left: ' ',
            top_right: ' ',
            bottom_left,
            bottom_right: ' ',
            horizontal: ' ',
            vertical,
        }
    }
}

#[derive(Clone)]
pub enum BorderStyle {
    None,
    Single,
    Double,
    Rounded,
    Thick,
    Custom(BorderChars),
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- BorderChars: 形状コンストラクタ ---
    // BorderChars は PartialEq を実装しないため、char フィールド単位で検証する。

    #[test]
    fn rounded_uses_arc_corner_glyphs() {
        let b = BorderChars::rounded();
        assert_eq!(b.top_left, '╭');
        assert_eq!(b.top_right, '╮');
        assert_eq!(b.bottom_left, '╰');
        assert_eq!(b.bottom_right, '╯');
        assert_eq!(b.horizontal, '─');
        assert_eq!(b.vertical, '│');
    }

    #[test]
    fn squared_uses_square_corner_glyphs() {
        let b = BorderChars::squared();
        assert_eq!(b.top_left, '┌');
        assert_eq!(b.top_right, '┐');
        assert_eq!(b.bottom_left, '└');
        assert_eq!(b.bottom_right, '┘');
        assert_eq!(b.horizontal, '─');
        assert_eq!(b.vertical, '│');
    }

    #[test]
    fn thick_uses_heavy_glyphs() {
        let b = BorderChars::thick();
        assert_eq!(b.top_left, '┏');
        assert_eq!(b.top_right, '┓');
        assert_eq!(b.bottom_left, '┗');
        assert_eq!(b.bottom_right, '┛');
        assert_eq!(b.horizontal, '━');
        assert_eq!(b.vertical, '┃');
    }

    #[test]
    fn default_matches_rounded_field_by_field() {
        let default = BorderChars::default();
        let rounded = BorderChars::rounded();
        assert_eq!(default.top_left, rounded.top_left);
        assert_eq!(default.top_right, rounded.top_right);
        assert_eq!(default.bottom_left, rounded.bottom_left);
        assert_eq!(default.bottom_right, rounded.bottom_right);
        assert_eq!(default.horizontal, rounded.horizontal);
        assert_eq!(default.vertical, rounded.vertical);
    }

    #[test]
    fn rounded_squared_thick_have_distinct_glyphs() {
        assert_ne!(
            BorderChars::rounded().top_left,
            BorderChars::squared().top_left
        );
        assert_ne!(
            BorderChars::squared().top_left,
            BorderChars::thick().top_left
        );
        assert_ne!(
            BorderChars::rounded().horizontal,
            BorderChars::thick().horizontal
        );
        assert_ne!(
            BorderChars::rounded().vertical,
            BorderChars::thick().vertical
        );
    }

    #[test]
    fn empty_and_custom_constructors_set_expected_fields() {
        let empty = BorderChars::empty();
        for c in [
            empty.top_left,
            empty.top_right,
            empty.bottom_left,
            empty.bottom_right,
            empty.horizontal,
            empty.vertical,
        ] {
            assert_eq!(c, ' ');
        }

        let custom = BorderChars::custom('│', '└');
        assert_eq!(custom.vertical, '│');
        assert_eq!(custom.bottom_left, '└');
        assert_eq!(custom.top_left, ' ');
        assert_eq!(custom.top_right, ' ');
        assert_eq!(custom.bottom_right, ' ');
        assert_eq!(custom.horizontal, ' ');
    }

    // --- BorderChars: Clone ---

    #[test]
    fn clone_preserves_all_fields_and_is_independent() {
        let original = BorderChars::custom('╎', '╘');
        let mut cloned = original.clone();

        assert_eq!(cloned.vertical, original.vertical);
        assert_eq!(cloned.bottom_left, original.bottom_left);
        assert_eq!(cloned.top_left, original.top_left);
        assert_eq!(cloned.top_right, original.top_right);
        assert_eq!(cloned.bottom_right, original.bottom_right);
        assert_eq!(cloned.horizontal, original.horizontal);

        // 値のコピーなので、クローンの変更は元に波及しない。
        cloned.vertical = 'X';
        assert_eq!(cloned.vertical, 'X');
        assert_eq!(original.vertical, '╎');
    }

    // --- BorderStyle: Clone ---

    #[test]
    fn border_style_clone_keeps_variant_and_custom_chars() {
        assert!(matches!(BorderStyle::None.clone(), BorderStyle::None));
        assert!(matches!(BorderStyle::Single.clone(), BorderStyle::Single));
        assert!(matches!(BorderStyle::Double.clone(), BorderStyle::Double));
        assert!(matches!(BorderStyle::Rounded.clone(), BorderStyle::Rounded));
        assert!(matches!(BorderStyle::Thick.clone(), BorderStyle::Thick));

        let custom = BorderStyle::Custom(BorderChars::thick());
        let cloned = custom.clone();
        match cloned {
            BorderStyle::Custom(chars) => {
                assert_eq!(chars.top_left, '┏');
                assert_eq!(chars.vertical, '┃');
            }
            _ => panic!("Custom バリアントが保持されませんでした"),
        }
    }
}
