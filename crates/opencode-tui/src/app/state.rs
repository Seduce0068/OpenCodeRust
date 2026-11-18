#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AppState {
    #[default]
    Running,
    Exiting,
    PromptFocused,
    DialogOpen,
    CommandPalette,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_state_default_is_running() {
        assert_eq!(AppState::default(), AppState::Running);
    }

    #[test]
    fn app_state_variants_are_all_distinct() {
        let variants = [
            AppState::Running,
            AppState::Exiting,
            AppState::PromptFocused,
            AppState::DialogOpen,
            AppState::CommandPalette,
        ];

        for (i, left) in variants.iter().enumerate() {
            for (j, right) in variants.iter().enumerate() {
                assert_eq!(left == right, i == j, "variants differ at {i}/{j}");
            }
        }
    }

    #[test]
    fn app_state_clone_preserves_each_variant() {
        for variant in [
            AppState::Running,
            AppState::Exiting,
            AppState::PromptFocused,
            AppState::DialogOpen,
            AppState::CommandPalette,
        ] {
            assert_eq!(variant.clone(), variant);
        }
    }
}
