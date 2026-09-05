//! A dependency-free JSON Schema **subset** validator for `tools/call`
//! arguments (#12).
//!
//! The policy runs inside `wasm32-wasip1`, where a full `jsonschema` crate is
//! heavyweight; the tool-input contracts we need to enforce use only a small,
//! well-understood slice of Draft-07. This validator covers that slice and
//! rejects anything it does not understand *permissively* (an unknown keyword
//! is ignored, never a hard failure) so a richer schema still validates on the
//! keywords it does support.
//!
//! Supported keywords: `type` (incl. a list of types), `properties`,
//! `required`, `additionalProperties` (bool or schema), `enum`, `minimum`,
//! `maximum`, `minLength`, `maxLength`, `items`, `minItems`, `maxItems`.
//!
//! ## Sanitized errors
//!
//! Error messages name the failing JSON pointer path and the constraint, and
//! for type/format mismatches report the *type* found — never the offending
//! value, which for a `tools/call` argument may carry sensitive caller input.
//! `enum`/bound values come from the (config-authored) schema, so echoing them
//! is safe.

use serde_json::Value;

/// Validate `instance` against `schema`. Returns `Ok(())` when valid, or a
/// single sanitized, human-readable error describing the first violation.
pub fn validate(instance: &Value, schema: &Value) -> Result<(), String> {
    validate_at(instance, schema, "<root>")
}

fn validate_at(instance: &Value, schema: &Value, path: &str) -> Result<(), String> {
    // A non-object schema (e.g. `true`) imposes no constraints.
    let Some(obj) = schema.as_object() else {
        return Ok(());
    };

    // -- type --------------------------------------------------------------
    if let Some(type_decl) = obj.get("type") {
        if !type_matches(instance, type_decl) {
            return Err(format!(
                "{path}: expected type {}, got {}",
                describe_type_decl(type_decl),
                json_type_name(instance)
            ));
        }
    }

    // -- enum --------------------------------------------------------------
    if let Some(Value::Array(allowed)) = obj.get("enum") {
        if !allowed.iter().any(|a| a == instance) {
            return Err(format!(
                "{path}: value is not one of the allowed options {}",
                render_enum(allowed)
            ));
        }
    }

    match instance {
        Value::Object(map) => {
            // -- required --------------------------------------------------
            if let Some(Value::Array(required)) = obj.get("required") {
                let missing: Vec<&str> = required
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|k| !map.contains_key(*k))
                    .collect();
                if !missing.is_empty() {
                    return Err(format!(
                        "{path}: missing required argument(s): {}",
                        missing.join(", ")
                    ));
                }
            }

            let properties = obj.get("properties").and_then(Value::as_object);

            // -- properties (recurse) --------------------------------------
            if let Some(props) = properties {
                for (key, subschema) in props {
                    if let Some(child) = map.get(key) {
                        validate_at(child, subschema, &child_path(path, key))?;
                    }
                }
            }

            // -- additionalProperties --------------------------------------
            if let Some(ap) = obj.get("additionalProperties") {
                let known = |k: &String| {
                    properties
                        .map(|p| p.contains_key(k.as_str()))
                        .unwrap_or(false)
                };
                match ap {
                    Value::Bool(false) => {
                        let extras: Vec<&str> = map
                            .keys()
                            .filter(|k| !known(k))
                            .map(String::as_str)
                            .collect();
                        if !extras.is_empty() {
                            return Err(format!(
                                "{path}: unexpected argument(s): {}",
                                extras.join(", ")
                            ));
                        }
                    }
                    // A schema: every extra property must validate against it.
                    schema_ap if schema_ap.is_object() => {
                        for (key, child) in map {
                            if !known(key) {
                                validate_at(child, schema_ap, &child_path(path, key))?;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Value::Array(items) => {
            if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
                if (items.len() as u64) < min {
                    return Err(format!(
                        "{path}: array has {} item(s), minimum is {min}",
                        items.len()
                    ));
                }
            }
            if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
                if (items.len() as u64) > max {
                    return Err(format!(
                        "{path}: array has {} item(s), maximum is {max}",
                        items.len()
                    ));
                }
            }
            if let Some(item_schema) = obj.get("items") {
                for (i, item) in items.iter().enumerate() {
                    validate_at(item, item_schema, &format!("{path}[{i}]"))?;
                }
            }
        }

        Value::String(s) => {
            let len = s.chars().count() as u64;
            if let Some(min) = obj.get("minLength").and_then(Value::as_u64) {
                if len < min {
                    return Err(format!(
                        "{path}: string length {len} is below minimum {min}"
                    ));
                }
            }
            if let Some(max) = obj.get("maxLength").and_then(Value::as_u64) {
                if len > max {
                    return Err(format!("{path}: string length {len} exceeds maximum {max}"));
                }
            }
        }

        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if let Some(min) = obj.get("minimum").and_then(Value::as_f64) {
                    if f < min {
                        return Err(format!("{path}: value is below minimum {min}"));
                    }
                }
                if let Some(max) = obj.get("maximum").and_then(Value::as_f64) {
                    if f > max {
                        return Err(format!("{path}: value exceeds maximum {max}"));
                    }
                }
            }
        }

        _ => {}
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn child_path(parent: &str, key: &str) -> String {
    if parent == "<root>" {
        key.to_string()
    } else {
        format!("{parent}.{key}")
    }
}

/// `type` may be a single string or an array of acceptable type names.
fn type_matches(instance: &Value, type_decl: &Value) -> bool {
    match type_decl {
        Value::String(t) => matches_one_type(instance, t),
        Value::Array(types) => types
            .iter()
            .filter_map(Value::as_str)
            .any(|t| matches_one_type(instance, t)),
        // Unrecognized `type` shape — don't block.
        _ => true,
    }
}

fn matches_one_type(instance: &Value, ty: &str) -> bool {
    match ty {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        "number" => instance.is_number(),
        "integer" => match instance {
            Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
            }
            _ => false,
        },
        // Unknown declared type — accept rather than block.
        _ => true,
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn describe_type_decl(type_decl: &Value) -> String {
    match type_decl {
        Value::String(t) => t.clone(),
        Value::Array(types) => {
            let names: Vec<&str> = types.iter().filter_map(Value::as_str).collect();
            format!("one of [{}]", names.join(", "))
        }
        _ => "the declared type".to_string(),
    }
}

/// Render an enum's allowed values compactly. These come from the schema
/// (config-authored), so they are safe to echo.
fn render_enum(allowed: &[Value]) -> String {
    let parts: Vec<String> = allowed.iter().map(|v| v.to_string()).collect();
    format!("[{}]", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "customerId": { "type": "string", "minLength": 1 },
                "quantity": { "type": "integer", "minimum": 1, "maximum": 100 },
                "tier": { "type": "string", "enum": ["gold", "silver"] },
                "tags": { "type": "array", "items": { "type": "string" }, "maxItems": 3 }
            },
            "required": ["customerId"],
            "additionalProperties": false
        })
    }

    #[test]
    fn accepts_a_valid_instance() {
        let inst = json!({"customerId": "CU-1", "quantity": 5, "tier": "gold", "tags": ["a"]});
        assert!(validate(&inst, &schema()).is_ok());
    }

    #[test]
    fn required_missing_is_named() {
        let err = validate(&json!({"quantity": 2}), &schema()).unwrap_err();
        assert!(err.contains("customerId"), "got: {err}");
    }

    #[test]
    fn wrong_type_reports_found_type_not_value() {
        let err = validate(&json!({"customerId": 123}), &schema()).unwrap_err();
        assert!(err.contains("customerId"));
        assert!(err.contains("string"));
        assert!(err.contains("number"));
        // Must not echo the offending value.
        assert!(!err.contains("123"));
    }

    #[test]
    fn integer_rejects_fractional_but_accepts_whole_float() {
        assert!(validate(&json!({"customerId": "c", "quantity": 2.5}), &schema()).is_err());
        assert!(validate(&json!({"customerId": "c", "quantity": 2.0}), &schema()).is_ok());
    }

    #[test]
    fn minimum_and_maximum_enforced() {
        assert!(validate(&json!({"customerId": "c", "quantity": 0}), &schema()).is_err());
        assert!(validate(&json!({"customerId": "c", "quantity": 101}), &schema()).is_err());
    }

    #[test]
    fn enum_rejects_unlisted_value() {
        let err = validate(&json!({"customerId": "c", "tier": "bronze"}), &schema()).unwrap_err();
        assert!(err.contains("tier"));
        assert!(err.contains("gold"));
    }

    #[test]
    fn min_length_enforced() {
        assert!(validate(&json!({"customerId": ""}), &schema()).is_err());
    }

    #[test]
    fn additional_properties_false_rejects_unknown_keys() {
        let err = validate(&json!({"customerId": "c", "sneaky": true}), &schema()).unwrap_err();
        assert!(err.contains("sneaky"), "got: {err}");
    }

    #[test]
    fn array_items_and_bounds_enforced() {
        // wrong item type
        assert!(validate(&json!({"customerId": "c", "tags": [1]}), &schema()).is_err());
        // too many items
        assert!(validate(
            &json!({"customerId": "c", "tags": ["a", "b", "c", "d"]}),
            &schema()
        )
        .is_err());
    }

    #[test]
    fn nested_error_path_is_reported() {
        let s = json!({
            "type": "object",
            "properties": {
                "address": {
                    "type": "object",
                    "properties": { "zip": { "type": "string" } }
                }
            }
        });
        let err = validate(&json!({"address": {"zip": 90210}}), &s).unwrap_err();
        assert!(err.contains("address.zip"), "got: {err}");
    }

    #[test]
    fn no_schema_constraints_accepts_anything() {
        // An empty schema (or a schema with no recognized keywords) validates.
        assert!(validate(&json!({"whatever": [1, 2, 3]}), &json!({})).is_ok());
    }

    #[test]
    fn unknown_keyword_is_ignored_not_failed() {
        let s = json!({"type": "object", "x-vendor-thing": {"weird": true}});
        assert!(validate(&json!({"any": "thing"}), &s).is_ok());
    }
}
