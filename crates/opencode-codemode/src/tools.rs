//! Host tool boundary for the confined interpreter.
//!
//! Upstream CodeMode resolves `tools.*` references against a host-provided tool
//! tree and invokes them asynchronously. The Rust port keeps the reference
//! model but uses a synchronous host: a tool call produces data immediately and
//! `await` passes that data through. Arguments and results cross the same
//! plain-data boundary as every other host interaction.
//!
//! A caller can pass a remaining wall-clock budget with
//! [`ToolHost::invoke_with_deadline`]. Because the boundary is synchronous, that
//! budget is advisory: a blocking host implementation must enforce it itself.

use std::cell::Cell;
use std::fmt::{self, Display, Formatter};

use serde_json::Value as JsonValue;

/// Budget used when the caller has no deadline to pass.
///
/// `u64::MAX` means "unbounded": the host is not asked to bound the call. This
/// is what the legacy [`ToolHost::invoke`] entry point forwards.
pub const DEFAULT_TOOL_TIMEOUT_MS: u64 = u64::MAX;

thread_local! {
    /// Breaks the mutual fallback between the two provided `invoke` methods so
    /// a host that overrides neither fails safely instead of recursing.
    static DEFAULT_INVOKE_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

struct DefaultInvokeGuard;

impl DefaultInvokeGuard {
    fn enter() -> Option<Self> {
        DEFAULT_INVOKE_ACTIVE.with(|active| {
            if active.replace(true) {
                None
            } else {
                Some(Self)
            }
        })
    }
}

impl Drop for DefaultInvokeGuard {
    fn drop(&mut self) {
        DEFAULT_INVOKE_ACTIVE.with(|active| active.set(false));
    }
}

/// A safe tool failure reported to the sandboxed program.
///
/// The message is surfaced to the model, so hosts must not include secrets or
/// raw internal diagnostics. Optional structured data is exposed as
/// `error.data` inside `catch` blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolError {
    message: String,
    data: Option<JsonValue>,
}

impl ToolError {
    /// Creates a failure with a model-safe message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            data: None,
        }
    }

    /// Creates a failure with a model-safe message and structured data.
    pub fn with_data(message: impl Into<String>, data: JsonValue) -> Self {
        Self {
            message: message.into(),
            data: Some(data),
        }
    }

    /// Model-safe failure message.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Optional structured failure data.
    pub fn data(&self) -> Option<&JsonValue> {
        self.data.as_ref()
    }
}

impl Display for ToolError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ToolError {}

/// Synchronous host interface for `tools.*` references.
///
/// `path` contains the namespace segments after the `tools` root. For
/// `tools.math.add`, it is `["math", "add"]`.
pub trait ToolHost {
    /// Returns the child namespace and tool names under `path`.
    ///
    /// Used by `Object.keys(tools)` and `Object.keys(tools.ns)`. Unknown paths
    /// return an empty list.
    fn keys(&self, path: &[String]) -> Vec<String> {
        let _ = path;
        Vec::new()
    }

    /// Invokes the tool at `path` with plain-data arguments and an explicit
    /// remaining wall-clock budget in milliseconds.
    ///
    /// `duration_ms` is the time still available to this call when the
    /// interpreter enters the host. [`DEFAULT_TOOL_TIMEOUT_MS`] means no
    /// deadline was configured. Returning `Err` makes the call throw inside the
    /// sandbox, so a program can handle it with `try`/`catch`.
    ///
    /// # Host obligations
    ///
    /// This trait is **synchronous**. Passing a budget cannot interrupt a host
    /// that is blocked in I/O, and the interpreter cannot abort such a call: it
    /// only checks the deadline before and after the call. A host therefore
    /// **must enforce the limit itself** (check its clock during I/O, use a
    /// bounded I/O API, etc.) when a hard wall-clock guarantee is required.
    /// Fully enforcing the deadline, including cancellation of a blocking call,
    /// would require making the host interface `async` with cancellation; that
    /// is out of scope here and is left as a separate proposal.
    ///
    /// The default implementation ignores `duration_ms` and delegates to
    /// [`ToolHost::invoke`], so existing hosts need no changes.
    fn invoke_with_deadline(
        &mut self,
        path: &[String],
        arguments: Vec<JsonValue>,
        duration_ms: u64,
    ) -> Result<JsonValue, ToolError> {
        let _ = duration_ms;
        self.invoke(path, arguments)
    }

    /// Invokes the tool at `path` using the default (unbounded) time budget.
    ///
    /// This is the legacy entry point kept as a wrapper that forwards
    /// [`DEFAULT_TOOL_TIMEOUT_MS`] to [`ToolHost::invoke_with_deadline`].
    /// Hosts that need a deadline override `invoke_with_deadline` instead.
    fn invoke(
        &mut self,
        path: &[String],
        arguments: Vec<JsonValue>,
    ) -> Result<JsonValue, ToolError> {
        let Some(_guard) = DefaultInvokeGuard::enter() else {
            return Err(ToolError::new(
                "ToolHost must implement `invoke` or `invoke_with_deadline`.",
            ));
        };
        self.invoke_with_deadline(path, arguments, DEFAULT_TOOL_TIMEOUT_MS)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::{json, Value as JsonValue};

    use super::{ToolError, ToolHost, DEFAULT_TOOL_TIMEOUT_MS};

    /// Host stub: no external execution, only a fixed plain-data result.
    struct DummyHost;

    impl ToolHost for DummyHost {
        fn invoke(
            &mut self,
            _path: &[String],
            _arguments: Vec<JsonValue>,
        ) -> Result<JsonValue, ToolError> {
            Ok(json!("dummy-result"))
        }
    }

    fn assert_std_error<T: std::error::Error>() {}

    #[test]
    fn tool_error_without_data_exposes_message_only() {
        let error = ToolError::new("safe failure");

        assert_eq!(error.message(), "safe failure");
        assert_eq!(error.data(), None);
        assert_eq!(error.to_string(), "safe failure");
        assert_eq!(error, ToolError::new("safe failure"));
        assert_ne!(error, ToolError::new("different failure"));
        assert_std_error::<ToolError>();
    }

    #[test]
    fn tool_error_with_data_keeps_structured_payload() {
        let payload = json!({ "code": 7, "items": [1, 2] });
        let error = ToolError::with_data("structured failure", payload.clone());

        assert_eq!(error.message(), "structured failure");
        assert_eq!(error.data(), Some(&payload));
        assert_eq!(error.to_string(), "structured failure");
        assert_eq!(error, ToolError::with_data("structured failure", payload));
    }

    #[test]
    fn tool_error_distinguishes_json_null_from_absent_data() {
        let error = ToolError::with_data("", JsonValue::Null);

        assert_eq!(error.message(), "");
        assert_eq!(error.data(), Some(&JsonValue::Null));
        assert_ne!(error, ToolError::new(""));
        assert!(error.to_string().is_empty());
    }

    #[test]
    fn default_keys_returns_empty_for_any_path() {
        let mut host = DummyHost;
        let path = vec!["math".to_string(), "add".to_string()];

        assert!(host.keys(&path).is_empty());
        assert!(host.keys(&[]).is_empty());

        let result = host
            .invoke(&path, vec![json!(1), json!(2)])
            .expect("dummy host returns a plain value");
        assert_eq!(result, json!("dummy-result"));

        // A legacy host that only implements `invoke` still receives calls made
        // through the new deadline-aware entry point.
        let result = host
            .invoke_with_deadline(&path, vec![json!(1)], 25)
            .expect("legacy host works through the overload");
        assert_eq!(result, json!("dummy-result"));
    }

    /// Host recording the budget handed to the deadline-aware entry point.
    struct BudgetHost(Rc<RefCell<Vec<u64>>>);

    impl ToolHost for BudgetHost {
        fn invoke_with_deadline(
            &mut self,
            _path: &[String],
            _arguments: Vec<JsonValue>,
            duration_ms: u64,
        ) -> Result<JsonValue, ToolError> {
            self.0.borrow_mut().push(duration_ms);
            Ok(json!("ok"))
        }
    }

    #[test]
    fn legacy_invoke_forwards_the_default_budget() {
        let budgets = Rc::new(RefCell::new(Vec::new()));
        let mut host = BudgetHost(Rc::clone(&budgets));

        assert_eq!(
            host.invoke(&[], vec![]).expect("host should respond"),
            json!("ok")
        );
        assert_eq!(*budgets.borrow(), vec![DEFAULT_TOOL_TIMEOUT_MS]);
    }
}
