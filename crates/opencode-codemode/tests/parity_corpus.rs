//! Differential corpus for CodeMode standard-library behavior.
//!
//! Cases without a `group` are dependency-free: expected values were produced
//! by Node.js and then reviewed for the places where upstream CodeMode
//! intentionally differs from plain JavaScript (UTC dates, plain-object Error
//! values).
//!
//! Cases tagged `"group": "regexp-url-normalize"` cover the dependency-backed
//! extensions (fancy-regex RegExp, the `url` crate's URL / URLSearchParams,
//! and `String.normalize`). No Node.js-generated fixtures exist for those
//! extensions yet, so their expected values were set by hand from the relevant
//! standards, never from this Rust implementation's own output:
//!
//! - RegExp: ECMA-262 lookahead / lookbehind / numeric-backreference semantics.
//!   Zero-width lookaround does not consume input, so the capture is the digits
//!   themselves; `\1` repeats the code unit captured by group 1.
//! - URL / URLSearchParams: WHATWG URL component serialization and
//!   application/x-www-form-urlencoded parsing. `searchParams.set` writes the
//!   updated query back to the owning URL.
//! - String.normalize: UAX #15 normalization forms. `a` + U+0301 composes to
//!   U+00E1 under NFC (also the no-argument default) and decomposes back under
//!   NFD; the ligature U+FB01 expands to `fi` under NFKC. An unknown form is a
//!   `RangeError`.
//!
//! Both tests read the same JSON file. The `group` field only selects which
//! subset each test exercises, so the provenance of every expectation stays
//! explicit while all cases are still executed.

use opencode_codemode::{execute_code, CodeModeResult, ExecutionLimits};

const DEPENDENCY_BACKED_GROUP: &str = "regexp-url-normalize";

/// Loads the shared JSON corpus.
fn load_cases() -> Vec<serde_json::Value> {
    serde_json::from_str(include_str!("parity_cases.json"))
        .expect("parity corpus must be valid JSON")
}

/// Normalizes integral floats to integers so `16.0` and `16` compare equal.
fn normalize(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(number) => match number.as_f64() {
            Some(float) if float.is_finite() && float.fract() == 0.0 => {
                serde_json::Value::Number(serde_json::Number::from(float as i64))
            }
            _ => value.clone(),
        },
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(normalize).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), normalize(item)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

/// Runs the cases selected by `group` (`None` selects only ungrouped cases).
fn check_cases(cases: &[serde_json::Value], group: Option<&str>) {
    let limits = ExecutionLimits::default();
    let mut failures = Vec::new();
    let mut checked = 0usize;

    for case in cases {
        let case_group = case.get("group").and_then(|value| value.as_str());
        let selected = match group {
            Some(expected) => case_group == Some(expected),
            None => case_group.is_none(),
        };
        if !selected {
            continue;
        }
        checked += 1;
        let expression = case["expr"].as_str().expect("expression");
        let expected = case.get("expected").expect("expected value");
        let source = format!("return {expression};");
        match execute_code(&source, None, &limits) {
            CodeModeResult::Success(success) => {
                if normalize(&success.value) != normalize(expected) {
                    failures.push(format!(
                        "{expression}\n  expected: {expected}\n  actual:   {}",
                        success.value
                    ));
                }
            }
            CodeModeResult::Failure(failure) => failures.push(format!(
                "{expression}\n  {}: {}",
                failure.error.kind, failure.error.message
            )),
        }
    }

    assert!(checked > 0, "no corpus cases selected for {group:?}");
    assert!(
        failures.is_empty(),
        "{} of {} selected corpus cases failed:\n{}",
        failures.len(),
        checked,
        failures.join("\n")
    );
}

#[test]
fn dependency_free_stdlib_matches_the_reference_corpus() {
    let cases = load_cases();
    check_cases(&cases, None);
}

#[test]
fn dependency_backed_stdlib_matches_the_reference_corpus() {
    let cases = load_cases();
    // A misspelled group must not leave a case unexecuted by both tests.
    assert!(
        cases.iter().all(|case| {
            case.get("group").is_none() || case["group"].as_str() == Some(DEPENDENCY_BACKED_GROUP)
        }),
        "unknown corpus group"
    );
    check_cases(&cases, Some(DEPENDENCY_BACKED_GROUP));
}
