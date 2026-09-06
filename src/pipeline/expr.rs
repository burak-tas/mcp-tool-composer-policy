//! Context-aware `${...}` expression interpolation for URL, JSON-body, and
//! header templates.
//!
//! Supported syntax:
//!   ${args.<field>}                 — top-level key from the tool's input arguments
//!   ${steps.<stepName>}             — shorthand output of a completed step
//!   ${steps.<stepName>.<dot.path>}  — nested field from a step's response body
//!
//! ## Injection-safe construction (#11)
//!
//! Substituted values are **never** spliced raw. Each interpolation site encodes
//! the resolved value for its surrounding syntax so a caller-controlled MCP
//! argument cannot alter the *structure* of the outbound request:
//!
//! - [`interpolate_url`] percent-encodes every substituted value (RFC 3986
//!   unreserved set only), so a value like `Berlin&count=100` cannot inject a
//!   query parameter and `../../admin` cannot traverse the path.
//! - [`interpolate_json_body`] is JSON-position aware: a value substituted inside
//!   a JSON string (`"${...}"`) is emitted as escaped string content, and a value
//!   in a bare value position (`${...}`) is emitted as a complete JSON token via
//!   `serde_json`. Either way a `"` or `\` cannot break out and inject sibling
//!   fields.
//! - [`interpolate_header`] strips CR/LF from substituted values to prevent
//!   header injection.
//!
//! ## Strict resolution (#11)
//!
//! Unlike the previous empty-string-on-miss behaviour, an expression that cannot
//! be resolved — a missing `args`/`steps` key, or an unsupported prefix such as
//! `${env.*}` — is a hard [`InterpError`]. The pipeline surfaces it as a failed
//! call rather than silently sending a request with a hole in it.

use serde_json::Value;
use std::collections::HashMap;

/// Resolved outputs keyed by step name.
pub type StepOutputs = HashMap<String, Value>;

/// An interpolation failure. Kept free of resolved *values* so it is always safe
/// to log or surface — only the expression text (config-authored, not secret) is
/// included.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InterpError {
    #[error("unresolved reference '${{{0}}}'")]
    Unresolved(String),

    #[error("unsupported expression prefix in '${{{0}}}' (only args.* and steps.* are supported)")]
    UnsupportedPrefix(String),

    #[error("malformed expression: missing '}}' after '${{'")]
    Malformed,
}

// ---------------------------------------------------------------------------
// Reference resolution
// ---------------------------------------------------------------------------

/// Resolve a single `${...}` expression body (already trimmed) to a JSON value.
/// A present-but-null value resolves to `&Value::Null`; a missing key or an
/// unsupported prefix is an error.
fn resolve_ref<'a>(
    expr: &str,
    args: &'a Value,
    step_outputs: &'a StepOutputs,
) -> Result<&'a Value, InterpError> {
    if let Some(rest) = expr.strip_prefix("args.") {
        args.get(rest)
            .ok_or_else(|| InterpError::Unresolved(expr.to_string()))
    } else if let Some(rest) = expr.strip_prefix("steps.") {
        let (step_name, path) = match rest.split_once('.') {
            Some((n, p)) => (n, Some(p)),
            None => (rest, None),
        };
        let root = step_outputs
            .get(step_name)
            .ok_or_else(|| InterpError::Unresolved(expr.to_string()))?;
        match path {
            None => Ok(root),
            Some(p) => {
                traverse(Some(root), p).ok_or_else(|| InterpError::Unresolved(expr.to_string()))
            }
        }
    } else {
        Err(InterpError::UnsupportedPrefix(expr.to_string()))
    }
}

/// The plain (unencoded) string form of a resolved value: strings are used
/// verbatim; every other JSON type uses its compact JSON representation.
fn value_to_plain_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Walk a dot-separated path through a JSON value. Returns `None` when any
/// segment is missing. Exposed for the pipeline executor's `outputExtract`.
pub fn traverse_value<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    traverse(Some(root), path)
}

/// Walk a dot-separated path through a JSON value. Numeric segments index into
/// arrays (e.g. `results.0.latitude`).
fn traverse<'a>(mut v: Option<&'a Value>, path: &str) -> Option<&'a Value> {
    for segment in path.split('.') {
        v = match v? {
            Value::Array(arr) => {
                let idx: usize = segment.parse().ok()?;
                arr.get(idx)
            }
            other => other.get(segment),
        };
    }
    v
}

// ---------------------------------------------------------------------------
// Encoders
// ---------------------------------------------------------------------------

/// RFC 3986 unreserved characters — the only bytes left un-encoded in a URL
/// component. Encoding everything else (including `/`, `?`, `#`, `&`, `=`,
/// space) is safe in both path segments and query components, so one encoder
/// serves the whole URL template regardless of where the value lands.
fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &byte in s.as_bytes() {
        if is_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(hex_upper(byte >> 4));
            out.push(hex_upper(byte & 0x0f));
        }
    }
    out
}

fn hex_upper(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        _ => (b'A' + (nibble - 10)) as char,
    }
}

/// Escape a string's content for embedding **inside** an existing JSON string
/// (no surrounding quotes). Uses `serde_json` for correctness and strips the
/// outer quotes it adds.
fn escape_json_string_inner(s: &str) -> String {
    let quoted = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string());
    // `quoted` is always `"..."` — drop the surrounding quotes.
    quoted[1..quoted.len() - 1].to_string()
}

/// Remove characters that could inject a new header line or break the current
/// one (CR, LF, and NUL).
fn strip_header_unsafe(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '\0'))
        .collect()
}

// ---------------------------------------------------------------------------
// Context-aware interpolation
// ---------------------------------------------------------------------------

/// A lexed span of a template: either literal text or a trimmed `${...}`
/// expression body.
enum Token<'a> {
    Literal(&'a str),
    Expr(&'a str),
}

/// Split `template` into literal and `${...}` tokens. A `${` with no closing
/// `}` is a hard error. Literal runs are coalesced so string-state tracking in
/// the body interpolator sees contiguous text.
fn lex(template: &str) -> Result<Vec<Token<'_>>, InterpError> {
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut lit_start = 0;
    while i < template.len() {
        if template[i..].starts_with("${") {
            if i > lit_start {
                tokens.push(Token::Literal(&template[lit_start..i]));
            }
            let rest = &template[i + 2..];
            let end = rest.find('}').ok_or(InterpError::Malformed)?;
            tokens.push(Token::Expr(rest[..end].trim()));
            i += 2 + end + 1;
            lit_start = i;
        } else {
            i += template[i..].chars().next().unwrap().len_utf8();
        }
    }
    if lit_start < template.len() {
        tokens.push(Token::Literal(&template[lit_start..]));
    }
    Ok(tokens)
}

/// Interpolate a URL path/query template. Every substituted value is
/// percent-encoded so it can only ever be data, never structure.
pub fn interpolate_url(
    template: &str,
    args: &Value,
    step_outputs: &StepOutputs,
) -> Result<String, InterpError> {
    let mut result = String::with_capacity(template.len());
    for token in lex(template)? {
        match token {
            Token::Literal(lit) => result.push_str(lit),
            Token::Expr(expr) => {
                let v = resolve_ref(expr, args, step_outputs)?;
                result.push_str(&percent_encode(&value_to_plain_string(v)));
            }
        }
    }
    Ok(result)
}

/// Interpolate a JSON body template, position-aware: a value inside a JSON
/// string is escaped string content; a value in a bare position is a complete
/// JSON token. Prevents structural breakout regardless of the caller's input.
pub fn interpolate_json_body(
    template: &str,
    args: &Value,
    step_outputs: &StepOutputs,
) -> Result<String, InterpError> {
    let mut result = String::with_capacity(template.len());
    // JSON string-literal state of the *literal template text* copied so far.
    let mut in_string = false;
    let mut escaped = false;

    for token in lex(template)? {
        match token {
            Token::Literal(lit) => {
                for c in lit.chars() {
                    if in_string {
                        if escaped {
                            escaped = false;
                        } else if c == '\\' {
                            escaped = true;
                        } else if c == '"' {
                            in_string = false;
                        }
                    } else if c == '"' {
                        in_string = true;
                    }
                    result.push(c);
                }
            }
            Token::Expr(expr) => {
                let v = resolve_ref(expr, args, step_outputs)?;
                if in_string {
                    result.push_str(&escape_json_string_inner(&value_to_plain_string(v)));
                } else {
                    // A complete, well-formed JSON token for the value.
                    result
                        .push_str(&serde_json::to_string(v).unwrap_or_else(|_| "null".to_string()));
                }
            }
        }
    }
    Ok(result)
}

/// Interpolate a header value (or an auth credential template). Substituted
/// values have CR/LF/NUL stripped to prevent header injection.
pub fn interpolate_header(
    template: &str,
    args: &Value,
    step_outputs: &StepOutputs,
) -> Result<String, InterpError> {
    let mut result = String::with_capacity(template.len());
    for token in lex(template)? {
        match token {
            Token::Literal(lit) => result.push_str(lit),
            Token::Expr(expr) => {
                let v = resolve_ref(expr, args, step_outputs)?;
                result.push_str(&strip_header_unsafe(&value_to_plain_string(v)));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_steps() -> StepOutputs {
        HashMap::new()
    }

    // -- URL context --------------------------------------------------------

    #[test]
    fn url_interpolates_simple_arg() {
        let args = json!({"customerId": "CU-42"});
        assert_eq!(
            interpolate_url("/customers/${args.customerId}", &args, &empty_steps()).unwrap(),
            "/customers/CU-42"
        );
    }

    #[test]
    fn url_percent_encodes_query_injection() {
        // The exact example from the issue: a `&`-bearing value must not inject
        // a second query parameter.
        let args = json!({"city": "Berlin&count=100"});
        assert_eq!(
            interpolate_url(
                "/v1/search?name=${args.city}&count=1",
                &args,
                &empty_steps()
            )
            .unwrap(),
            "/v1/search?name=Berlin%26count%3D100&count=1"
        );
    }

    #[test]
    fn url_percent_encodes_path_traversal_and_slashes() {
        let args = json!({"id": "../../admin"});
        assert_eq!(
            interpolate_url("/users/${args.id}", &args, &empty_steps()).unwrap(),
            "/users/..%2F..%2Fadmin"
        );
    }

    #[test]
    fn url_percent_encodes_spaces_and_unicode() {
        let args = json!({"city": "São Paulo"});
        assert_eq!(
            interpolate_url("/q?name=${args.city}", &args, &empty_steps()).unwrap(),
            "/q?name=S%C3%A3o%20Paulo"
        );
    }

    #[test]
    fn url_leaves_literal_separators_untouched() {
        let args = json!({"a": "1", "b": "2"});
        assert_eq!(
            interpolate_url("/p/${args.a}/q?x=${args.b}&y=z", &args, &empty_steps()).unwrap(),
            "/p/1/q?x=2&y=z"
        );
    }

    // -- JSON body context --------------------------------------------------

    #[test]
    fn body_escapes_string_context_quote_breakout() {
        // A quote-bearing value must stay contained inside its JSON string.
        let args = json!({"customerId": r#"","admin":true,"x":""#});
        let out = interpolate_json_body(
            r#"{"customerId":"${args.customerId}"}"#,
            &args,
            &empty_steps(),
        )
        .unwrap();
        // Result must parse and preserve exactly one, correctly-typed field.
        let parsed: Value = serde_json::from_str(&out).expect("body must be valid JSON");
        assert_eq!(parsed["customerId"], r#"","admin":true,"x":""#);
        assert!(
            parsed.get("admin").is_none(),
            "must not inject sibling field"
        );
    }

    #[test]
    fn body_backslash_is_escaped() {
        let args = json!({"path": r"a\b"});
        let out = interpolate_json_body(r#"{"p":"${args.path}"}"#, &args, &empty_steps()).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["p"], r"a\b");
    }

    #[test]
    fn body_bare_position_emits_json_token() {
        // Bare (unquoted) position: a number stays a number, a string becomes a
        // properly quoted JSON string.
        let args = json!({"quantity": 5, "note": "hi\"there"});
        let out = interpolate_json_body(
            r#"{"quantity":${args.quantity},"note":${args.note}}"#,
            &args,
            &empty_steps(),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["quantity"], 5);
        assert_eq!(parsed["note"], "hi\"there");
    }

    #[test]
    fn body_bare_position_emits_nested_object() {
        let mut steps = StepOutputs::new();
        steps.insert("lookup".into(), json!({"id": "ORD-1", "tags": ["a", "b"]}));
        let out = interpolate_json_body(r#"{"echo":${steps.lookup}}"#, &json!({}), &steps).unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["echo"]["id"], "ORD-1");
        assert_eq!(parsed["echo"]["tags"][1], "b");
    }

    #[test]
    fn body_mixed_string_and_bare_matches_example_convention() {
        // Mirrors playground/example-order-flow.yaml.
        let args = json!({"customerId": "CU-1", "productSku": "SKU-9", "quantity": 3});
        let out = interpolate_json_body(
            r#"{"customerId":"${args.customerId}","sku":"${args.productSku}","quantity":${args.quantity}}"#,
            &args,
            &empty_steps(),
        )
        .unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["customerId"], "CU-1");
        assert_eq!(parsed["sku"], "SKU-9");
        assert_eq!(parsed["quantity"], 3);
    }

    #[test]
    fn body_null_value_is_preserved() {
        let mut steps = StepOutputs::new();
        steps.insert("prev".into(), Value::Null);
        // Present-but-null resolves (not an error); bare → `null`.
        let out = interpolate_json_body(r#"{"v":${steps.prev}}"#, &json!({}), &steps).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap()["v"],
            Value::Null
        );
    }

    // -- Header context -----------------------------------------------------

    #[test]
    fn header_strips_crlf_injection() {
        let args = json!({"v": "abc\r\nX-Injected: 1"});
        assert_eq!(
            interpolate_header("${args.v}", &args, &empty_steps()).unwrap(),
            "abcX-Injected: 1"
        );
    }

    // -- Strict resolution (#11): errors, not empty strings -----------------

    #[test]
    fn missing_arg_is_an_error() {
        let err = interpolate_url("${args.missing}", &json!({}), &empty_steps()).unwrap_err();
        assert!(matches!(err, InterpError::Unresolved(_)));
    }

    #[test]
    fn missing_step_is_an_error() {
        let err = interpolate_json_body(r#"{"x":"${steps.nope}"}"#, &json!({}), &empty_steps())
            .unwrap_err();
        assert!(matches!(err, InterpError::Unresolved(_)));
    }

    #[test]
    fn unsupported_prefix_is_an_error() {
        let err = interpolate_url("${env.SECRET}", &json!({}), &empty_steps()).unwrap_err();
        assert!(matches!(err, InterpError::UnsupportedPrefix(_)));
    }

    #[test]
    fn malformed_expression_is_an_error() {
        let err = interpolate_url("${args.x", &json!({}), &empty_steps()).unwrap_err();
        assert_eq!(err, InterpError::Malformed);
    }

    #[test]
    fn error_message_contains_no_resolved_value() {
        // Only the expression text (config-authored) appears — never data.
        let err = interpolate_url("${args.token}", &json!({"other": "s3cr3t"}), &empty_steps())
            .unwrap_err();
        assert!(!err.to_string().contains("s3cr3t"));
    }

    // -- Resolution mechanics (ported from the original suite) --------------

    #[test]
    fn interpolates_step_output_shorthand() {
        let mut steps = StepOutputs::new();
        steps.insert("lookup".into(), json!("ID-99"));
        assert_eq!(
            interpolate_url("/${steps.lookup}", &json!({}), &steps).unwrap(),
            "/ID-99"
        );
    }

    #[test]
    fn interpolates_step_nested_field() {
        let mut steps = StepOutputs::new();
        steps.insert("create".into(), json!({"data": {"id": "ORD-1"}}));
        assert_eq!(
            interpolate_url("/${steps.create.data.id}", &json!({}), &steps).unwrap(),
            "/ORD-1"
        );
    }

    #[test]
    fn interpolates_array_index() {
        let mut steps = StepOutputs::new();
        steps.insert(
            "geo".into(),
            json!({"results": [{"latitude": 41.01, "longitude": 28.94}]}),
        );
        assert_eq!(
            interpolate_url("/${steps.geo.results.0.latitude}", &json!({}), &steps).unwrap(),
            "/41.01"
        );
    }

    #[test]
    fn multiple_expressions_in_one_template() {
        let args = json!({"a": "X", "b": "Y"});
        assert_eq!(
            interpolate_url("/${args.a}-${args.b}", &args, &empty_steps()).unwrap(),
            "/X-Y"
        );
    }

    #[test]
    fn literal_passthrough_when_no_expressions() {
        assert_eq!(
            interpolate_url("/static/path", &json!({}), &empty_steps()).unwrap(),
            "/static/path"
        );
    }
}
