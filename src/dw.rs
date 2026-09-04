//! Utilities for evaluating PDK DataWeave scripts against serde_json values.

use pdk::script::{Script, Value as DwValue};
use serde_json::Value;

/// Evaluate an optional DataWeave script with the JSON `input` bound as the
/// `payload` (matches the `Input::Payload` declaration in the generated script
/// deserializers). Returns `Ok(transformed)` on success, `Err(message)` when
/// evaluation fails so the caller can surface a deterministic error rather than
/// silently continuing with the wrong shape.
///
/// When `script` is `None` the input is returned unchanged.
pub fn eval_transform(script: Option<&Script>, input: &Value) -> Result<Value, String> {
    let Some(script) = script else {
        return Ok(input.clone());
    };

    let mut evaluator = script.evaluator();

    // `String` implements `PayloadBinding` — bind the serialized JSON as the
    // top-level `payload`, matching the `Input::Payload(Format::Json)` declared
    // when the script was compiled.
    let payload_str = serde_json::to_string(input)
        .map_err(|e| format!("failed to serialize transform input: {e}"))?;
    evaluator.bind_payload(&payload_str);

    match evaluator.eval() {
        Ok(DwValue::Null) => Ok(input.clone()),
        Ok(dw_val) => Ok(dw_to_json(dw_val)),
        Err(e) => Err(format!("transform evaluation error: {e}")),
    }
}

/// Convert a `pdk::script::Value` to `serde_json::Value`.
fn dw_to_json(v: DwValue) -> Value {
    match v {
        DwValue::Null => Value::Null,
        DwValue::Bool(b) => Value::Bool(b),
        DwValue::Number(n) => {
            if let Ok(i) = n.to_string().parse::<i64>() {
                Value::Number(i.into())
            } else if let Ok(f) = n.to_string().parse::<f64>() {
                serde_json::Number::from_f64(f)
                    .map(Value::Number)
                    .unwrap_or(Value::Null)
            } else {
                Value::String(n.to_string())
            }
        }
        DwValue::String(s) => Value::String(s),
        DwValue::Array(arr) => Value::Array(arr.into_iter().map(dw_to_json).collect()),
        DwValue::Object(obj) => Value::Object(
            obj.into_iter().map(|(k, v)| (k, dw_to_json(v))).collect(),
        ),
    }
}
