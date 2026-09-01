//! Lightweight `${...}` expression interpolation for path and body templates.
//!
//! Supported syntax:
//!   ${args.<field>}           — top-level key from the tool's input arguments
//!   ${steps.<stepName>}       — shorthand output of a completed step
//!   ${steps.<stepName>.<dot.path>} — nested field from a step's response body
//!
//! Substitution is string-based: values are serialized to JSON (strings
//! are unquoted, numbers/booleans/null use their JSON representation).
//! An expression that resolves to a missing key is replaced with the empty
//! string rather than failing, keeping error handling in the caller.

use serde_json::Value;
use std::collections::HashMap;

/// Resolved outputs keyed by step name.
pub type StepOutputs = HashMap<String, Value>;

/// Interpolate all `${...}` expressions in `template` against `args` and
/// `step_outputs`. Returns the interpolated string.
pub fn interpolate(
    template: &str,
    args: &Value,
    step_outputs: &StepOutputs,
) -> String {
    let mut result = String::with_capacity(template.len());
    let mut remaining = template;

    while let Some(start) = remaining.find("${") {
        result.push_str(&remaining[..start]);
        remaining = &remaining[start + 2..];
        if let Some(end) = remaining.find('}') {
            let expr = &remaining[..end];
            remaining = &remaining[end + 1..];
            result.push_str(&resolve_expr(expr.trim(), args, step_outputs));
        } else {
            // Malformed expression — emit literally and stop scanning.
            result.push_str("${");
            result.push_str(remaining);
            break;
        }
    }
    result.push_str(remaining);
    result
}

fn resolve_expr(expr: &str, args: &Value, step_outputs: &StepOutputs) -> String {
    if let Some(rest) = expr.strip_prefix("args.") {
        json_to_string(args.get(rest))
    } else if let Some(rest) = expr.strip_prefix("steps.") {
        // rest is either "<stepName>" or "<stepName>.<dot.path>"
        let (step_name, path) = match rest.split_once('.') {
            Some((n, p)) => (n, Some(p)),
            None => (rest, None),
        };
        let root = step_outputs.get(step_name);
        match path {
            None => json_to_string(root),
            Some(p) => json_to_string(traverse(root, p)),
        }
    } else {
        String::new()
    }
}

/// Walk a dot-separated path through a JSON value. Returns `None` when
/// any segment is missing. Exposed for use by the pipeline executor.
pub fn traverse_value<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    traverse(Some(root), path)
}

/// Walk a dot-separated path through a JSON value. Returns `None` when
/// any segment is missing. Numeric segments index into arrays (e.g. `results.0.latitude`).
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

fn json_to_string(v: Option<&Value>) -> String {
    match v {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn empty_steps() -> StepOutputs {
        HashMap::new()
    }

    #[test]
    fn test_interpolates_args() {
        let args = json!({"customerId": "CU-42"});
        let result = interpolate("/customers/${args.customerId}", &args, &empty_steps());
        assert_eq!(result, "/customers/CU-42");
    }

    #[test]
    fn test_interpolates_step_output_shorthand() {
        let args = json!({});
        let mut steps = StepOutputs::new();
        steps.insert("lookup".into(), json!("ID-99"));
        let result = interpolate("${steps.lookup}", &args, &steps);
        assert_eq!(result, "ID-99");
    }

    #[test]
    fn test_interpolates_step_nested_field() {
        let args = json!({});
        let mut steps = StepOutputs::new();
        steps.insert("create".into(), json!({"data": {"id": "ORD-1"}}));
        let result = interpolate("${steps.create.data.id}", &args, &steps);
        assert_eq!(result, "ORD-1");
    }

    #[test]
    fn test_missing_arg_becomes_empty_string() {
        let args = json!({});
        let result = interpolate("${args.missing}", &args, &empty_steps());
        assert_eq!(result, "");
    }

    #[test]
    fn test_literal_passthrough_when_no_expressions() {
        let args = json!({});
        let result = interpolate("/static/path", &args, &empty_steps());
        assert_eq!(result, "/static/path");
    }

    #[test]
    fn test_interpolates_array_index() {
        let args = json!({});
        let mut steps = StepOutputs::new();
        steps.insert(
            "geo".into(),
            json!({"results": [{"latitude": 41.01, "longitude": 28.94}]}),
        );
        assert_eq!(
            interpolate("${steps.geo.results.0.latitude}", &args, &steps),
            "41.01"
        );
    }

    #[test]
    fn test_multiple_expressions_in_one_template() {
        let args = json!({"a": "X", "b": "Y"});
        let result = interpolate("${args.a}-${args.b}", &args, &empty_steps());
        assert_eq!(result, "X-Y");
    }
}
