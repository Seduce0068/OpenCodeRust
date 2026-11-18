use crossterm::event::{KeyEvent, MouseEvent};

#[derive(Clone, Debug)]
pub enum Event {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Resize(u16, u16),
    Tick,
    FocusGained,
    FocusLost,
    Paste(String),
    Custom(CustomEvent),
}

#[derive(Clone, Debug)]
pub enum CustomEvent {
    Message(String),
    StreamChunk(String),
    StreamComplete,
    StreamError(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallComplete {
        id: String,
        result: String,
    },
    StateChanged(StateChange),
    QuestionAsked(crate::api::QuestionRequestInfo),
    PermissionRequested {
        id: String,
        permission_type: String,
        resource: String,
        tool_name: String,
    },
}

#[derive(Clone, Debug)]
pub enum StateChange {
    SessionCreated(String),
    SessionUpdated(String),
    SessionStatusBusy(String),
    SessionStatusIdle(String),
    SessionStatusRetrying {
        session_id: String,
        attempt: u32,
        message: String,
        next: i64,
    },
    SessionDeleted(String),
    ModelChanged(String),
    AgentChanged(String),
    ProviderConnected(String),
    ProviderDisconnected(String),
    McpServerStatusChanged {
        name: String,
        status: String,
    },
    TodoUpdated,
    DiffUpdated,
}

pub struct EventBus {
    tx: std::sync::mpsc::Sender<Event>,
}

impl Clone for EventBus {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = std::sync::mpsc::channel();
        Self { tx }
    }

    pub fn sender(&self) -> std::sync::mpsc::Sender<Event> {
        self.tx.clone()
    }

    pub fn send(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn send_custom(&self, event: CustomEvent) {
        let _ = self.tx.send(Event::Custom(event));
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
    use std::sync::mpsc;

    // チャネルをテスト内で保持するため、スレッド spawn なしで同期配送を検証できる。
    fn bus_with_receiver() -> (EventBus, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        (EventBus { tx }, rx)
    }

    // 送信は受信より前に完了しているため、ブロックしない try_recv で十分。
    fn recv_event(rx: &mpsc::Receiver<Event>) -> Event {
        rx.try_recv().expect("event should already be queued")
    }

    fn roundtrip_custom(event: CustomEvent) -> CustomEvent {
        let (bus, rx) = bus_with_receiver();
        bus.send_custom(event);
        match recv_event(&rx) {
            Event::Custom(custom) => custom,
            other => panic!("expected Event::Custom, got {other:?}"),
        }
    }

    #[test]
    fn send_custom_delivers_custom_event_wrapped_in_event() {
        let (bus, rx) = bus_with_receiver();
        bus.send_custom(CustomEvent::Message("hello".to_string()));

        match recv_event(&rx) {
            Event::Custom(CustomEvent::Message(text)) => assert_eq!(text, "hello"),
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "only one event must be delivered");
    }

    #[test]
    fn send_delivers_event_unchanged() {
        let (bus, rx) = bus_with_receiver();
        bus.send(Event::Resize(120, 40));

        match recv_event(&rx) {
            Event::Resize(cols, rows) => {
                assert_eq!(cols, 120);
                assert_eq!(rows, 40);
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "only one event must be delivered");
    }

    #[test]
    fn key_and_mouse_events_carry_payloads_without_terminal_io() {
        let (bus, rx) = bus_with_receiver();
        bus.send(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        )));
        bus.send(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 7,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }));

        match recv_event(&rx) {
            Event::Key(actual) => {
                assert_eq!(actual.code, KeyCode::Char('x'));
                assert!(actual.modifiers.contains(KeyModifiers::CONTROL));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        match recv_event(&rx) {
            Event::Mouse(actual) => {
                assert_eq!(actual.column, 7);
                assert_eq!(actual.row, 3);
                assert!(matches!(
                    actual.kind,
                    MouseEventKind::Down(MouseButton::Left)
                ));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn simple_event_variants_carry_their_payloads() {
        let (bus, rx) = bus_with_receiver();
        bus.send(Event::Paste("clipboard".to_string()));
        bus.send(Event::FocusGained);
        bus.send(Event::FocusLost);
        bus.send(Event::Tick);

        match recv_event(&rx) {
            Event::Paste(text) => assert_eq!(text, "clipboard"),
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(matches!(recv_event(&rx), Event::FocusGained));
        assert!(matches!(recv_event(&rx), Event::FocusLost));
        assert!(matches!(recv_event(&rx), Event::Tick));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn send_custom_preserves_stream_variant_payloads() {
        match roundtrip_custom(CustomEvent::StreamChunk("tok".to_string())) {
            CustomEvent::StreamChunk(chunk) => assert_eq!(chunk, "tok"),
            other => panic!("unexpected custom event: {other:?}"),
        }
        assert!(matches!(
            roundtrip_custom(CustomEvent::StreamComplete),
            CustomEvent::StreamComplete
        ));
        match roundtrip_custom(CustomEvent::StreamError("boom".to_string())) {
            CustomEvent::StreamError(err) => assert_eq!(err, "boom"),
            other => panic!("unexpected custom event: {other:?}"),
        }
    }

    #[test]
    fn send_custom_preserves_tool_call_fields() {
        match roundtrip_custom(CustomEvent::ToolCallStart {
            id: "call-1".to_string(),
            name: "read_file".to_string(),
        }) {
            CustomEvent::ToolCallStart { id, name } => {
                assert_eq!(id, "call-1");
                assert_eq!(name, "read_file");
            }
            other => panic!("unexpected custom event: {other:?}"),
        }

        match roundtrip_custom(CustomEvent::ToolCallComplete {
            id: "call-2".to_string(),
            result: "ok".to_string(),
        }) {
            CustomEvent::ToolCallComplete { id, result } => {
                assert_eq!(id, "call-2");
                assert_eq!(result, "ok");
            }
            other => panic!("unexpected custom event: {other:?}"),
        }
    }

    #[test]
    fn send_custom_preserves_nested_state_change() {
        let custom = roundtrip_custom(CustomEvent::StateChanged(
            StateChange::SessionStatusRetrying {
                session_id: "session-1".to_string(),
                attempt: 3,
                message: "retry".to_string(),
                next: 1_700_000_000,
            },
        ));

        match custom {
            CustomEvent::StateChanged(StateChange::SessionStatusRetrying {
                session_id,
                attempt,
                message,
                next,
            }) => {
                assert_eq!(session_id, "session-1");
                assert_eq!(attempt, 3);
                assert_eq!(message, "retry");
                assert_eq!(next, 1_700_000_000);
            }
            other => panic!("unexpected custom event: {other:?}"),
        }
    }

    #[test]
    fn send_custom_preserves_question_and_permission_requests() {
        let request = crate::api::QuestionRequestInfo {
            id: "question-1".to_string(),
            session_id: "session-1".to_string(),
            questions: Vec::new(),
        };
        match roundtrip_custom(CustomEvent::QuestionAsked(request)) {
            CustomEvent::QuestionAsked(actual) => {
                assert_eq!(actual.id, "question-1");
                assert_eq!(actual.session_id, "session-1");
                assert!(actual.questions.is_empty());
            }
            other => panic!("unexpected custom event: {other:?}"),
        }

        match roundtrip_custom(CustomEvent::PermissionRequested {
            id: "permission-1".to_string(),
            permission_type: "edit".to_string(),
            resource: "/tmp/file".to_string(),
            tool_name: "write".to_string(),
        }) {
            CustomEvent::PermissionRequested {
                id,
                permission_type,
                resource,
                tool_name,
            } => {
                assert_eq!(id, "permission-1");
                assert_eq!(permission_type, "edit");
                assert_eq!(resource, "/tmp/file");
                assert_eq!(tool_name, "write");
            }
            other => panic!("unexpected custom event: {other:?}"),
        }
    }

    #[test]
    fn sender_clones_share_the_same_channel() {
        let (bus, rx) = bus_with_receiver();
        let first = bus.sender();
        let second = first.clone();
        first.send(Event::Tick).expect("send via first sender");
        second
            .send(Event::FocusGained)
            .expect("send via cloned sender");

        assert!(matches!(recv_event(&rx), Event::Tick));
        assert!(matches!(recv_event(&rx), Event::FocusGained));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn cloned_event_bus_shares_the_same_receiver() {
        let (bus, rx) = bus_with_receiver();
        let cloned = bus.clone();
        bus.send(Event::Paste("a".to_string()));
        cloned.send(Event::Paste("b".to_string()));

        match recv_event(&rx) {
            Event::Paste(text) => assert_eq!(text, "a"),
            other => panic!("unexpected event: {other:?}"),
        }
        match recv_event(&rx) {
            Event::Paste(text) => assert_eq!(text, "b"),
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn send_without_receiver_is_silently_ignored() {
        // 本番と同じく new()/default() は受信側を保持しないため、Err が握り潰される。
        let bus = EventBus::new();
        bus.send(Event::Tick);
        bus.send_custom(CustomEvent::Message("ignored".to_string()));

        let default_bus = EventBus::default();
        default_bus.send(Event::FocusLost);
        default_bus.send_custom(CustomEvent::StreamComplete);
    }

    #[test]
    fn state_change_variants_are_cloneable_and_keep_variant_identity() {
        let cases = [
            (StateChange::SessionCreated("s".into()), "SessionCreated"),
            (StateChange::SessionUpdated("s".into()), "SessionUpdated"),
            (
                StateChange::SessionStatusBusy("s".into()),
                "SessionStatusBusy",
            ),
            (
                StateChange::SessionStatusIdle("s".into()),
                "SessionStatusIdle",
            ),
            (
                StateChange::SessionStatusRetrying {
                    session_id: "s".into(),
                    attempt: 1,
                    message: "m".into(),
                    next: 0,
                },
                "SessionStatusRetrying",
            ),
            (StateChange::SessionDeleted("s".into()), "SessionDeleted"),
            (StateChange::ModelChanged("m".into()), "ModelChanged"),
            (StateChange::AgentChanged("a".into()), "AgentChanged"),
            (
                StateChange::ProviderConnected("p".into()),
                "ProviderConnected",
            ),
            (
                StateChange::ProviderDisconnected("p".into()),
                "ProviderDisconnected",
            ),
            (
                StateChange::McpServerStatusChanged {
                    name: "mcp".into(),
                    status: "up".into(),
                },
                "McpServerStatusChanged",
            ),
            (StateChange::TodoUpdated, "TodoUpdated"),
            (StateChange::DiffUpdated, "DiffUpdated"),
        ];

        for (variant, expected_name) in cases {
            let debug = format!("{variant:?}");
            assert!(
                debug.starts_with(expected_name),
                "unexpected debug representation: {debug}"
            );
            assert_eq!(format!("{:?}", variant.clone()), debug);
        }
    }
}
