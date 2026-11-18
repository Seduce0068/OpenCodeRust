pub const APP_NAME: &str = "OpenCodeRust";
pub const APP_SHORT_NAME: &str = "opencoderust";
pub const APP_VERSION_DATE: &str = "2026.02.23";
pub const APP_TAGLINE: &str = "A Rust implementation of OpenCode";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branding_name_constants_have_expected_values() {
        assert_eq!(APP_NAME, "OpenCodeRust");
        assert_eq!(APP_SHORT_NAME, "opencoderust");
        assert_eq!(APP_SHORT_NAME, APP_NAME.to_lowercase());
    }

    #[test]
    fn branding_version_date_is_dot_separated_numeric() {
        assert_eq!(APP_VERSION_DATE, "2026.02.23");
        assert_eq!(APP_VERSION_DATE.len(), 10);
        assert_eq!(APP_VERSION_DATE.matches('.').count(), 2);

        let segments: Vec<&str> = APP_VERSION_DATE.split('.').collect();
        assert_eq!(segments, ["2026", "02", "23"]);
        for segment in segments {
            assert!(segment.len() >= 2);
            assert!(segment.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn branding_tagline_describes_the_rust_implementation() {
        assert_eq!(APP_TAGLINE, "A Rust implementation of OpenCode");
        assert!(APP_TAGLINE.contains("Rust"));
        assert!(APP_TAGLINE.contains("OpenCode"));
    }

    #[test]
    fn branding_constants_are_all_non_empty() {
        for value in [APP_NAME, APP_SHORT_NAME, APP_VERSION_DATE, APP_TAGLINE] {
            assert!(!value.is_empty());
        }
    }
}
