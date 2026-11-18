use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Route {
    #[default]
    Home,
    Session {
        session_id: String,
    },
    Settings,
    Help,
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Route::Home => write!(f, "Home"),
            Route::Session { session_id } => write!(f, "Session: {}", session_id),
            Route::Settings => write!(f, "Settings"),
            Route::Help => write!(f, "Help"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Router {
    history: Vec<Route>,
    current: Route,
}

impl Router {
    pub fn new() -> Self {
        Self {
            history: vec![Route::Home],
            current: Route::Home,
        }
    }

    pub fn current(&self) -> &Route {
        &self.current
    }

    pub fn navigate(&mut self, route: Route) {
        if self.current != route {
            self.history.push(route.clone());
            self.current = route;
        }
    }

    pub fn go_back(&mut self) -> bool {
        if self.history.len() > 1 {
            self.history.pop();
            if let Some(prev) = self.history.last() {
                self.current = prev.clone();
                return true;
            }
        }
        false
    }

    pub fn is_home(&self) -> bool {
        matches!(self.current, Route::Home)
    }

    pub fn is_session(&self) -> bool {
        matches!(self.current, Route::Session { .. })
    }

    pub fn session_id(&self) -> Option<&str> {
        match &self.current {
            Route::Session { session_id } => Some(session_id),
            _ => None,
        }
    }
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> Route {
        Route::Session {
            session_id: id.to_string(),
        }
    }

    // --- Route ---

    #[test]
    fn route_default_is_home() {
        assert_eq!(Route::default(), Route::Home);
    }

    #[test]
    fn route_display_formats_all_variants() {
        assert_eq!(Route::Home.to_string(), "Home");
        assert_eq!(session("abc-123").to_string(), "Session: abc-123");
        assert_eq!(Route::Settings.to_string(), "Settings");
        assert_eq!(Route::Help.to_string(), "Help");
    }

    // --- Router::new / current / is_home / is_session ---

    #[test]
    fn router_new_starts_at_home_with_home_in_history() {
        let router = Router::new();

        assert_eq!(router.current(), &Route::Home);
        assert_eq!(router.history, [Route::Home]);
        assert!(router.is_home());
        assert!(!router.is_session());
        assert_eq!(router.session_id(), None);
    }

    #[test]
    fn router_default_current_is_home() {
        let router = Router::default();

        assert_eq!(router.current(), &Route::Home);
        assert!(router.is_home());
    }

    #[test]
    fn router_default_matches_new_history_and_current() {
        let from_default = Router::default();
        let from_new = Router::new();

        assert_eq!(from_default.history, [Route::Home]);
        assert_eq!(from_default.current(), &Route::Home);
        assert_eq!(from_default.history, from_new.history);
        assert_eq!(from_default.current(), from_new.current());
    }

    // --- Router::navigate ---

    #[test]
    fn router_navigate_updates_current_and_records_history() {
        let mut router = Router::new();
        router.navigate(session("s-1"));

        assert_eq!(router.current(), &session("s-1"));
        assert!(router.is_session());
        assert!(!router.is_home());
        assert_eq!(router.session_id(), Some("s-1"));
        assert_eq!(router.history, [Route::Home, session("s-1")]);
    }

    #[test]
    fn router_navigate_to_same_route_is_no_op() {
        let mut router = Router::new();
        router.navigate(Route::Home);
        assert_eq!(router.history, [Route::Home]);

        router.navigate(session("same"));
        router.navigate(session("same"));

        assert_eq!(router.current(), &session("same"));
        assert_eq!(router.history, [Route::Home, session("same")]);
        assert!(router.go_back());
        assert!(router.is_home());
        assert!(!router.go_back());
    }

    // --- Router::go_back ---

    #[test]
    fn router_go_back_from_home_returns_false() {
        let mut router = Router::new();

        assert!(!router.go_back());
        assert_eq!(router.current(), &Route::Home);
        assert_eq!(router.history, [Route::Home]);
    }

    #[test]
    fn router_go_back_restores_previous_route_and_returns_true() {
        let mut router = Router::new();
        router.navigate(session("s-1"));
        router.navigate(Route::Settings);

        assert_eq!(router.current(), &Route::Settings);
        assert!(router.go_back());
        assert_eq!(router.current(), &session("s-1"));
        assert!(router.is_session());
        assert_eq!(router.session_id(), Some("s-1"));
    }

    #[test]
    fn router_go_back_walks_full_history_then_stops_at_home() {
        let mut router = Router::new();
        router.navigate(session("s-1"));
        router.navigate(Route::Help);
        router.navigate(Route::Settings);

        assert!(router.go_back());
        assert_eq!(router.current(), &Route::Help);
        assert!(router.go_back());
        assert_eq!(router.current(), &session("s-1"));
        assert!(router.go_back());
        assert_eq!(router.current(), &Route::Home);
        assert!(router.is_home());
        assert!(!router.go_back());
        assert_eq!(router.history, [Route::Home]);
    }

    // --- is_home / is_session across route kinds ---

    #[test]
    fn router_flags_are_mutually_exclusive_across_route_kinds() {
        let mut router = Router::new();
        assert!(router.is_home());
        assert!(!router.is_session());

        router.navigate(session("s-1"));
        assert!(!router.is_home());
        assert!(router.is_session());

        router.navigate(Route::Settings);
        assert!(!router.is_home());
        assert!(!router.is_session());
        assert_eq!(router.session_id(), None);

        router.navigate(Route::Help);
        assert!(!router.is_home());
        assert!(!router.is_session());

        router.navigate(Route::Home);
        assert!(router.is_home());
        assert!(!router.is_session());
    }

    #[test]
    fn router_session_id_tracks_only_the_current_session_route() {
        let mut router = Router::new();
        assert_eq!(router.session_id(), None);

        router.navigate(session("session-42"));
        assert_eq!(router.session_id(), Some("session-42"));

        router.navigate(Route::Settings);
        assert_eq!(router.session_id(), None);

        assert!(router.go_back());
        assert_eq!(router.session_id(), Some("session-42"));
    }
}
