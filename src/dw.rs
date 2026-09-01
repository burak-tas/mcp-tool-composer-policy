//! Utilities for evaluating PDK DataWeave scripts against serde_json values.
//!
//! `Script` is `Option<pdk::script::Script>` — None means the transform was
//! not configured (or defaulted to "#[payload]").  In both cases we fall back
//! to returning the input unchanged so a missing/null transform is a no-op.

use pdk::script::{Script, Value as DwValue};
use serde_json::Value;

/// Evaluate an optional DataWeave script with `payload` bound to `input`.
///
/// Returns the transformed value on success, or `input` unchanged when the
/// script is None, evaluates to null, or errors.
pub fn eval_transform(script: Option<&Script>, input: &Value) -> Value {
    let Some(script) = script else {
        return input.clone();
    };

    let mut evaluator = script.evaluator();

    // Serialize the serde_json value to a JSON string and bind as a vars entry.
    // bind_payload() needs a RequestBodyState/ResponseBodyState handle which is
    // not available here; bind_vars with the JSON string is the correct approach
    // for out-of-band payload injection.
    let payload_str = match serde_json::to_string(input) {
        Ok(s) => s,
        Err(_) => return input.clone(),
    };
    evaluator.bind_vars("payload", payload_str);

    match evaluator.eval() {
        Ok(DwValue::Null) => input.clone(),
        Ok(dw_val) => dw_to_json(dw_val),
        Err(_) => input.clone(),
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
