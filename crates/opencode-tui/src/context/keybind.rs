use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const LEADER_TIMEOUT_MS: u64 = 2000;

pub struct LeaderKeyState {
    pub active: bool,
    pub start_time: Option<Instant>,
    pub leader_key: Option<KeyCode>,
}

impl LeaderKeyState {
    pub fn new() -> Self {
        Self {
            active: false,
            start_time: None,
            leader_key: None,
        }
    }

    pub fn start(&mut self, key: KeyCode) {
        self.active = true;
        self.start_time = Some(Instant::now());
        self.leader_key = Some(key);
    }

    pub fn check_timeout(&mut self) -> bool {
        if let Some(start) = self.start_time {
            if start.elapsed() > Duration::from_millis(LEADER_TIMEOUT_MS) {
                self.reset();
                return true;
            }
        }
        false
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.start_time = None;
        self.leader_key = None;
    }
}

impl Default for LeaderKeyState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Keybind {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Keybind {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub fn key(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::empty(),
        }
    }

    pub fn ctrl(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::CONTROL,
        }
    }

    pub fn alt(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::ALT,
        }
    }

    pub fn shift(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::SHIFT,
        }
    }

    pub fn ctrl_shift(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        }
    }
}

impl std::fmt::Display for Keybind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();

        if self.modifiers.contains(KeyModifiers::CONTROL) {
            parts.push("ctrl");
        }
        if self.modifiers.contains(KeyModifiers::ALT) {
            parts.push("alt");
        }
        if self.modifiers.contains(KeyModifiers::SHIFT) {
            parts.push("shift");
        }

        let key_str = match self.code {
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "enter".to_string(),
            KeyCode::Backspace => "backspace".to_string(),
            KeyCode::Esc => "esc".to_string(),
            KeyCode::Tab => "tab".to_string(),
            KeyCode::BackTab => "shift+tab".to_string(),
            KeyCode::Delete => "del".to_string(),
            KeyCode::Home => "home".to_string(),
            KeyCode::End => "end".to_string(),
            KeyCode::PageUp => "pgup".to_string(),
            KeyCode::PageDown => "pgdn".to_string(),
            KeyCode::Up => "up".to_string(),
            KeyCode::Down => "down".to_string(),
            KeyCode::Left => "left".to_string(),
            KeyCode::Right => "right".to_string(),
            KeyCode::F(n) => format!("f{}", n),
            _ => "?".to_string(),
        };
        parts.push(&key_str);

        write!(f, "{}", parts.join("+"))
    }
}

pub struct KeybindRegistry {
    bindings: HashMap<String, Keybind>,
}

impl Default for KeybindRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl KeybindRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            bindings: HashMap::new(),
        };
        registry.register_defaults();
        registry
    }

    fn register_defaults(&mut self) {
        self.register("app_exit", Keybind::ctrl(KeyCode::Char('c')));
        self.register("app_exit_alt", Keybind::key(KeyCode::Esc));

        self.register("input_submit", Keybind::key(KeyCode::Enter));
        self.register("input_newline", Keybind::alt(KeyCode::Enter));
        self.register("input_clear", Keybind::ctrl(KeyCode::Char('u')));
        self.register("input_paste", Keybind::ctrl(KeyCode::Char('v')));
        self.register("input_copy", Keybind::ctrl_shift(KeyCode::Char('c')));
        self.register("input_cut", Keybind::ctrl_shift(KeyCode::Char('x')));

        self.register("history_previous", Keybind::alt(KeyCode::Up));
        self.register("history_next", Keybind::alt(KeyCode::Down));

        self.register("command_list", Keybind::ctrl(KeyCode::Char('x')));
        self.register("command_palette", Keybind::ctrl(KeyCode::Char('p')));

        self.register("agent_cycle", Keybind::key(KeyCode::Tab));
        self.register("agent_cycle_reverse", Keybind::key(KeyCode::BackTab));
        self.register("model_cycle", Keybind::ctrl(KeyCode::Char('m')));
        self.register("variant_cycle", Keybind::ctrl(KeyCode::Char('v')));

        self.register("session_parent", Keybind::ctrl(KeyCode::Char('o')));
        self.register("session_child_cycle", Keybind::ctrl(KeyCode::Char('j')));
        self.register(
            "session_child_cycle_reverse",
            Keybind::ctrl(KeyCode::Char('k')),
        );
        self.register("session_rename", Keybind::ctrl(KeyCode::Char('r')));
        self.register("session_delete", Keybind::ctrl(KeyCode::Char('d')));
        self.register("session_interrupt", Keybind::key(KeyCode::Esc));

        self.register("sidebar_toggle", Keybind::ctrl(KeyCode::Char('s')));
        self.register("help_toggle", Keybind::ctrl(KeyCode::Char('h')));

        self.register("editor_open", Keybind::ctrl(KeyCode::Char('e')));

        self.register("cursor_up", Keybind::key(KeyCode::Up));
        self.register("cursor_down", Keybind::key(KeyCode::Down));
        self.register("cursor_left", Keybind::key(KeyCode::Left));
        self.register("cursor_right", Keybind::key(KeyCode::Right));

        self.register("page_up", Keybind::key(KeyCode::PageUp));
        self.register("page_down", Keybind::key(KeyCode::PageDown));
        self.register("home", Keybind::key(KeyCode::Home));
        self.register("end", Keybind::key(KeyCode::End));
    }

    pub fn register(&mut self, name: &str, keybind: Keybind) {
        self.bindings.insert(name.to_string(), keybind);
    }

    pub fn get(&self, name: &str) -> Option<&Keybind> {
        self.bindings.get(name)
    }

    pub fn match_key(&self, name: &str, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(kb) = self.bindings.get(name) else {
            return false;
        };

        if kb.code == code && kb.modifiers == modifiers {
            return true;
        }

        // Terminal implementations can report Shift+Tab as either:
        // - KeyCode::BackTab with no modifiers
        // - KeyCode::Tab with SHIFT modifier
        if kb.code == KeyCode::BackTab {
            match (code, modifiers) {
                (KeyCode::BackTab, mods) if mods.is_empty() || mods == KeyModifiers::SHIFT => {
                    return true;
                }
                (KeyCode::Tab, mods) if mods.contains(KeyModifiers::SHIFT) => {
                    return true;
                }
                _ => {}
            }
        }

        false
    }

    pub fn print(&self, name: &str) -> String {
        self.bindings
            .get(name)
            .map(|kb| kb.to_string())
            .unwrap_or_else(|| "?".to_string())
    }

    pub fn all(&self) -> &HashMap<String, Keybind> {
        &self.bindings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- LeaderKeyState ---

    #[test]
    fn leader_state_new_and_default_are_inactive() {
        for state in [LeaderKeyState::new(), LeaderKeyState::default()] {
            assert!(!state.active);
            assert!(state.start_time.is_none());
            assert!(state.leader_key.is_none());
        }
    }

    #[test]
    fn leader_state_start_sets_active_time_and_key() {
        let mut state = LeaderKeyState::new();
        state.start(KeyCode::Char('x'));

        assert!(state.active);
        assert!(state.start_time.is_some());
        assert_eq!(state.leader_key, Some(KeyCode::Char('x')));
    }

    #[test]
    fn leader_state_start_refreshes_timer() {
        let mut state = LeaderKeyState::new();
        state.start(KeyCode::Char('a'));
        // Force the recorded start far into the past without waiting.
        state.start_time = Some(Instant::now() - Duration::from_secs(3600));

        state.start(KeyCode::Char('b'));

        assert_eq!(state.leader_key, Some(KeyCode::Char('b')));
        // A fresh start resets the timer, so it must not be expired anymore.
        assert!(!state.check_timeout());
        assert!(state.active);
    }

    #[test]
    fn leader_state_reset_clears_all_fields() {
        let mut state = LeaderKeyState::new();
        state.start(KeyCode::Char('x'));

        state.reset();

        assert!(!state.active);
        assert!(state.start_time.is_none());
        assert!(state.leader_key.is_none());
    }

    #[test]
    fn leader_state_check_timeout_without_start_returns_false() {
        let mut state = LeaderKeyState::new();

        assert!(!state.check_timeout());
        assert!(!state.active);
        assert!(state.start_time.is_none());
    }

    #[test]
    fn leader_state_check_timeout_before_deadline_returns_false() {
        let mut state = LeaderKeyState::new();
        state.start(KeyCode::Char('x'));
        // Set the start time directly so the test never depends on wall-clock waiting.
        state.start_time = Some(Instant::now());

        assert!(!state.check_timeout());
        assert!(state.active);
        assert_eq!(state.leader_key, Some(KeyCode::Char('x')));
        assert!(state.start_time.is_some());
    }

    #[test]
    fn leader_state_check_timeout_after_deadline_returns_true_and_resets() {
        let mut state = LeaderKeyState::new();
        state.start(KeyCode::Char('x'));
        // Move the recorded start safely past the deadline (no sleep involved).
        state.start_time = Some(Instant::now() - Duration::from_millis(LEADER_TIMEOUT_MS + 500));

        assert!(state.check_timeout());
        assert!(!state.active);
        assert!(state.start_time.is_none());
        assert!(state.leader_key.is_none());
    }

    // --- Keybind ---

    #[test]
    fn keybind_constructors_set_code_and_modifiers() {
        let code = KeyCode::Char('a');

        assert_eq!(
            Keybind::new(code, KeyModifiers::ALT).modifiers,
            KeyModifiers::ALT
        );
        assert_eq!(Keybind::key(code).modifiers, KeyModifiers::empty());
        assert_eq!(Keybind::ctrl(code).modifiers, KeyModifiers::CONTROL);
        assert_eq!(Keybind::alt(code).modifiers, KeyModifiers::ALT);
        assert_eq!(Keybind::shift(code).modifiers, KeyModifiers::SHIFT);
        assert_eq!(
            Keybind::ctrl_shift(code).modifiers,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        );

        for bind in [
            Keybind::new(code, KeyModifiers::ALT),
            Keybind::key(code),
            Keybind::ctrl(code),
            Keybind::alt(code),
            Keybind::shift(code),
            Keybind::ctrl_shift(code),
        ] {
            assert_eq!(bind.code, code);
        }
    }

    #[test]
    fn keybind_equality_depends_on_code_and_modifiers() {
        assert_eq!(
            Keybind::ctrl(KeyCode::Char('c')),
            Keybind::ctrl(KeyCode::Char('c'))
        );
        assert_ne!(
            Keybind::ctrl(KeyCode::Char('c')),
            Keybind::alt(KeyCode::Char('c'))
        );
        assert_ne!(
            Keybind::ctrl(KeyCode::Char('c')),
            Keybind::ctrl(KeyCode::Char('d'))
        );
    }

    #[test]
    fn keybind_display_formats_modifiers_and_keys() {
        assert_eq!(Keybind::ctrl(KeyCode::Char('c')).to_string(), "ctrl+c");
        assert_eq!(
            Keybind::ctrl_shift(KeyCode::Char('c')).to_string(),
            "ctrl+shift+c"
        );
        assert_eq!(Keybind::key(KeyCode::Enter).to_string(), "enter");
        assert_eq!(Keybind::alt(KeyCode::Enter).to_string(), "alt+enter");
        assert_eq!(Keybind::key(KeyCode::BackTab).to_string(), "shift+tab");
        assert_eq!(Keybind::key(KeyCode::F(5)).to_string(), "f5");
        assert_eq!(Keybind::key(KeyCode::Char('z')).to_string(), "z");
        // Modifier order is fixed: ctrl, alt, shift.
        let all = Keybind::new(
            KeyCode::Char('a'),
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        assert_eq!(all.to_string(), "ctrl+alt+shift+a");
    }

    #[test]
    fn keybind_display_unknown_key_is_question_mark() {
        assert_eq!(Keybind::key(KeyCode::CapsLock).to_string(), "?");
    }

    // --- KeybindRegistry ---

    #[test]
    fn registry_new_contains_default_bindings() {
        let registry = KeybindRegistry::new();

        assert_eq!(
            registry.get("app_exit"),
            Some(&Keybind::ctrl(KeyCode::Char('c')))
        );
        assert_eq!(
            registry.get("input_submit"),
            Some(&Keybind::key(KeyCode::Enter))
        );
        assert!(registry.get("does_not_exist").is_none());
        assert!(!registry.all().is_empty());
        assert!(registry.all().contains_key("app_exit"));
    }

    #[test]
    fn registry_default_and_new_have_same_bindings() {
        let new = KeybindRegistry::new();
        let default = KeybindRegistry::default();

        assert_eq!(new.all(), default.all());
    }

    #[test]
    fn registry_register_overrides_existing_binding() {
        let mut registry = KeybindRegistry::new();
        registry.register("app_exit", Keybind::key(KeyCode::Char('q')));

        assert_eq!(
            registry.get("app_exit"),
            Some(&Keybind::key(KeyCode::Char('q')))
        );
        assert!(registry.match_key("app_exit", KeyCode::Char('q'), KeyModifiers::empty()));
        assert!(!registry.match_key("app_exit", KeyCode::Char('c'), KeyModifiers::CONTROL));
    }

    #[test]
    fn registry_match_key_compares_code_and_modifiers_exactly() {
        let registry = KeybindRegistry::new();

        assert!(registry.match_key("app_exit", KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(!registry.match_key("app_exit", KeyCode::Char('c'), KeyModifiers::empty()));
        assert!(!registry.match_key("app_exit", KeyCode::Char('x'), KeyModifiers::CONTROL));
        assert!(!registry.match_key("unknown", KeyCode::Char('c'), KeyModifiers::CONTROL));
    }

    #[test]
    fn registry_match_key_accepts_backtab_reporting_variants() {
        let registry = KeybindRegistry::new();
        let name = "agent_cycle_reverse";

        assert!(registry.match_key(name, KeyCode::BackTab, KeyModifiers::empty()));
        assert!(registry.match_key(name, KeyCode::BackTab, KeyModifiers::SHIFT));
        assert!(registry.match_key(name, KeyCode::Tab, KeyModifiers::SHIFT));
        // A plain Tab is a different binding (agent_cycle), not the reverse one.
        assert!(!registry.match_key(name, KeyCode::Tab, KeyModifiers::empty()));
        assert!(!registry.match_key(name, KeyCode::BackTab, KeyModifiers::CONTROL));
    }

    #[test]
    fn registry_print_formats_known_and_unknown_bindings() {
        let registry = KeybindRegistry::new();

        assert_eq!(registry.print("app_exit"), "ctrl+c");
        assert_eq!(registry.print("input_newline"), "alt+enter");
        assert_eq!(registry.print("agent_cycle_reverse"), "shift+tab");
        assert_eq!(registry.print("missing"), "?");
    }
}
