//! Typed, validated view over the policy configuration.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::Value;

use crate::generated::config::{
    Calls0Config as CallConfig, Config, Headers0Config as HeadersConfig,
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("stages must contain at least one entry")]
    NoStages,

    #[error("pipeline rejected: {0}")]
    Invalid(String),

    #[error("toolName is required and must not be empty")]
    MissingToolName,

    #[error("unknown authType '{0}' on call '{1}'")]
    UnknownAuthType(String, String),

    #[error("toolInputSchema is not valid JSON: {0}")]
    InvalidInputSchema(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn parse(s: &str, call: &str) -> Result<Self, ConfigError> {
        match s.to_uppercase().as_str() {
            "GET" => Ok(Self::Get),
            "POST" => Ok(Self::Post),
            "PUT" => Ok(Self::Put),
            "PATCH" => Ok(Self::Patch),
            "DELETE" => Ok(Self::Delete),
            other => Err(ConfigError::Invalid(format!(
                "call '{call}': unknown method '{other}'"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// Per-call authentication strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthConfig {
    /// No auth header added.
    None,
    /// Forward the Authorization header from the incoming MCP request as-is.
    Passthrough,
    /// Static or ${steps.*}-resolved Bearer token.
    Bearer { token: String },
    /// Static or ${steps.*}-resolved Basic credentials.
    Basic { username: String, password: String },
    /// Static or ${steps.*}-resolved API key header.
    ApiKey {
        header_name: String,
        api_key: String,
    },
    /// All auth comes from the `headers` array (fully interpolated).
    CustomHeaders,
}

impl AuthConfig {
    pub fn parse(call_name: &str, raw: &CallConfig) -> Result<Self, ConfigError> {
        let kind = raw.auth_type.as_deref().unwrap_or("none");
        match kind {
            "none" => Ok(Self::None),
            "passthrough" => Ok(Self::Passthrough),
            "bearerToken" => {
                let token = nonempty(raw.token.clone()).ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "call '{call_name}': authType=bearerToken requires non-empty 'token'"
                    ))
                })?;
                Ok(Self::Bearer { token })
            }
            "basicAuth" => {
                let username = nonempty(raw.username.clone()).ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "call '{call_name}': authType=basicAuth requires non-empty 'username'"
                    ))
                })?;
                let password = nonempty(raw.password.clone()).ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "call '{call_name}': authType=basicAuth requires non-empty 'password'"
                    ))
                })?;
                Ok(Self::Basic { username, password })
            }
            "apiKeyHeader" => {
                let header_name = nonempty(raw.header_name.clone()).ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "call '{call_name}': authType=apiKeyHeader requires non-empty 'headerName'"
                    ))
                })?;
                let api_key = nonempty(raw.api_key.clone()).ok_or_else(|| {
                    ConfigError::Invalid(format!(
                        "call '{call_name}': authType=apiKeyHeader requires non-empty 'apiKey'"
                    ))
                })?;
                Ok(Self::ApiKey {
                    header_name,
                    api_key,
                })
            }
            "customHeaders" => Ok(Self::CustomHeaders),
            other => Err(ConfigError::UnknownAuthType(
                other.to_string(),
                call_name.to_string(),
            )),
        }
    }

    /// Build the auth header pair from resolved (post-interpolation) values.
    /// For Passthrough and CustomHeaders this returns None — handled by the caller.
    pub fn auth_header_resolved(&self, resolved_token: &str) -> Option<(String, String)> {
        match self {
            Self::None | Self::Passthrough | Self::CustomHeaders => None,
            Self::Bearer { .. } => {
                Some(("authorization".into(), format!("Bearer {resolved_token}")))
            }
            Self::Basic { username, password } => {
                let encoded = B64.encode(format!("{username}:{password}").as_bytes());
                Some(("authorization".into(), format!("Basic {encoded}")))
            }
            Self::ApiKey { header_name, .. } => {
                Some((header_name.clone(), resolved_token.to_string()))
            }
        }
    }

    /// The raw (pre-interpolation) credential string, if any.
    /// For Bearer this is the token template; for ApiKey it is the key template.
    /// Basic auth credentials are stored directly on the enum variant — use
    /// `auth_header_resolved` with an empty string for those.
    pub fn raw_credential(&self) -> Option<&str> {
        match self {
            Self::Bearer { token } => Some(token.as_str()),
            Self::ApiKey { api_key, .. } => Some(api_key.as_str()),
            _ => None,
        }
    }
}

/// One validated REST call definition.
#[derive(Debug, Clone)]
pub struct CallDef {
    pub name: String,
    pub method: HttpMethod,
    pub path: String,
    pub body_template: Option<String>,
    pub auth: AuthConfig,
    pub extra_headers: Vec<(String, String)>,
    pub timeout_ms: Option<u32>,
    pub stop_on_error: bool,
    pub output_extract: Option<String>,
    pub mask_in_output: bool,
}

/// A stage is either one sequential call or a group of parallel calls.
#[derive(Debug, Clone)]
pub enum Stage {
    Sequential(CallDef),
    Parallel(Vec<CallDef>),
}

impl Stage {
    pub fn calls(&self) -> &[CallDef] {
        match self {
            Stage::Sequential(c) => std::slice::from_ref(c),
            Stage::Parallel(cs) => cs.as_slice(),
        }
    }
}

/// Top-level validated policy configuration (sans the DataWeave Scripts,
/// which live on the raw `Config` and are passed separately).
#[derive(Debug, Clone)]
pub struct PolicyConfig {
    pub mcp_endpoint: String,
    pub strict_mode: bool,
    pub tool_name: String,
    pub tool_description: String,
    pub tool_input_schema: Value,
    pub stages: Vec<Stage>,
    pub per_request_timeout_ms: u32,
    /// Global wall-clock cap for the entire pipeline (all stages combined).
    pub pipeline_timeout_ms: u32,
}

impl PolicyConfig {
    pub fn from_config(raw: &Config) -> Result<Self, ConfigError> {
        if raw.stages.is_empty() {
            return Err(ConfigError::NoStages);
        }
        if raw.stages.len() > 10 {
            return Err(ConfigError::Invalid(format!(
                "too many stages ({}) — maximum is 10",
                raw.stages.len()
            )));
        }

        let tool_name =
            nonempty(Some(raw.tool_name.clone())).ok_or(ConfigError::MissingToolName)?;

        let tool_input_schema = match &raw.tool_input_schema {
            None => serde_json::json!({"type": "object", "properties": {}}),
            Some(s) => serde_json::from_str(s)
                .map_err(|e| ConfigError::InvalidInputSchema(e.to_string()))?,
        };

        let per_request_timeout_ms = clamp_u32(raw.per_request_timeout_ms, 100, 600_000, 30_000);

        // Global pipeline deadline: default 60 s, max 600 s (same ceiling as per-call).
        let pipeline_timeout_ms = clamp_u32(raw.pipeline_timeout_ms, 1_000, 600_000, 60_000);

        let mut all_call_names: Vec<String> = Vec::new();
        let mut total_calls: usize = 0;
        let mut stages = Vec::with_capacity(raw.stages.len());

        for (si, raw_stage) in raw.stages.iter().enumerate() {
            let parallel = raw_stage.parallel.unwrap_or(false);

            if raw_stage.calls.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "stage[{si}]: must contain at least one call"
                )));
            }
            if !parallel && raw_stage.calls.len() > 1 {
                return Err(ConfigError::Invalid(format!(
                    "stage[{si}]: sequential stage must have exactly one call, found {}",
                    raw_stage.calls.len()
                )));
            }
            if parallel && raw_stage.calls.len() > 5 {
                return Err(ConfigError::Invalid(format!(
                    "stage[{si}]: parallel stage may have at most 5 calls, found {}",
                    raw_stage.calls.len()
                )));
            }

            total_calls += raw_stage.calls.len();
            if total_calls > 10 {
                return Err(ConfigError::Invalid(
                    "total calls across all stages exceeds maximum of 10".into(),
                ));
            }

            let call_defs = parse_calls(si, &raw_stage.calls, &mut all_call_names)?;

            // Validate: parallel calls must not reference sibling outputs in any
            // interpolated field (path, body, auth credentials, custom headers).
            if parallel {
                let sibling_names: Vec<&str> = call_defs.iter().map(|c| c.name.as_str()).collect();
                for call in &call_defs {
                    for sibling in &sibling_names {
                        if call.name == *sibling {
                            continue;
                        }
                        let ref_pat = format!("${{steps.{}", sibling);

                        // Check EVERY interpolated field independently (#11) — a
                        // call may carry both a credential and a header, so these
                        // are not mutually exclusive.
                        let bad_field: Option<&str> = if call.path.contains(&ref_pat) {
                            Some("path")
                        } else if call
                            .body_template
                            .as_deref()
                            .map(|t| t.contains(&ref_pat))
                            .unwrap_or(false)
                        {
                            Some("bodyTemplate")
                        } else if call
                            .auth
                            .raw_credential()
                            .map(|cred| cred.contains(&ref_pat))
                            .unwrap_or(false)
                        {
                            Some("auth credential")
                        } else if call
                            .extra_headers
                            .iter()
                            .any(|(_, hv)| hv.contains(&ref_pat))
                        {
                            Some("header value")
                        } else {
                            None
                        };

                        if let Some(field) = bad_field {
                            return Err(ConfigError::Invalid(format!(
                                "stage[{si}] call '{}': {} references sibling '{}' inside same parallel stage",
                                call.name, field, sibling
                            )));
                        }
                    }
                }
            }

            let stage = if parallel {
                Stage::Parallel(call_defs)
            } else {
                Stage::Sequential(call_defs.into_iter().next().unwrap())
            };
            stages.push(stage);
        }

        Ok(Self {
            mcp_endpoint: normalize_mcp_path(raw.mcp_endpoint.as_deref().unwrap_or("/mcp")),
            strict_mode: raw.strict_mode.unwrap_or(true),
            tool_name,
            tool_description: raw.tool_description.clone(),
            tool_input_schema,
            stages,
            per_request_timeout_ms,
            pipeline_timeout_ms,
        })
    }

    pub fn all_calls(&self) -> impl Iterator<Item = &CallDef> {
        self.stages.iter().flat_map(|s| s.calls().iter())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_calls(
    stage_idx: usize,
    raw_calls: &[CallConfig],
    all_names: &mut Vec<String>,
) -> Result<Vec<CallDef>, ConfigError> {
    let mut defs = Vec::with_capacity(raw_calls.len());
    for (ci, raw_call) in raw_calls.iter().enumerate() {
        let name = raw_call.name.trim();
        if name.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "stage[{stage_idx}] call[{ci}]: missing/empty 'name'"
            )));
        }
        if all_names.iter().any(|n| n == name) {
            return Err(ConfigError::Invalid(format!(
                "duplicate call name '{name}' — names must be unique across all stages"
            )));
        }
        all_names.push(name.to_string());

        let method = HttpMethod::parse(raw_call.method.as_deref().unwrap_or("POST"), name)?;
        let path = normalize_path(raw_call.path.as_deref().unwrap_or("/"));
        let auth = AuthConfig::parse(name, raw_call)?;
        let extra_headers = parse_headers(name, raw_call.headers.as_deref())?;
        let timeout_ms = raw_call
            .timeout_ms
            .filter(|v| (100..=600_000).contains(v))
            .map(|v| v as u32);

        defs.push(CallDef {
            name: name.to_string(),
            method,
            path,
            body_template: raw_call.body_template.clone(),
            auth,
            extra_headers,
            timeout_ms,
            stop_on_error: raw_call.stop_on_error.unwrap_or(true),
            output_extract: raw_call.output_extract.clone(),
            mask_in_output: raw_call.mask_in_output.unwrap_or(false),
        });
    }
    Ok(defs)
}

fn parse_headers(
    call_name: &str,
    rows: Option<&[HeadersConfig]>,
) -> Result<Vec<(String, String)>, ConfigError> {
    let Some(rows) = rows else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let name = row.name.trim();
        if name.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "call '{call_name}' header[{i}]: missing/empty 'name'"
            )));
        }
        out.push((name.to_string(), row.value.clone()));
    }
    Ok(out)
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.trim().is_empty())
}

fn normalize_mcp_path(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return "/mcp".into();
    }
    if !s.starts_with('/') {
        return format!("/{s}");
    }
    s.to_string()
}

fn normalize_path(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || s == "/" {
        return "/".into();
    }
    if !s.starts_with('/') {
        return format!("/{s}");
    }
    s.to_string()
}

fn clamp_u32(v: Option<i64>, min: i64, max: i64, default: i64) -> u32 {
    v.unwrap_or(default).clamp(min, max) as u32
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_method_parses_case_insensitive() {
        assert_eq!(HttpMethod::parse("get", "s").unwrap(), HttpMethod::Get);
        assert_eq!(HttpMethod::parse("POST", "s").unwrap(), HttpMethod::Post);
        assert!(HttpMethod::parse("NOPE", "s").is_err());
    }

    #[test]
    fn test_normalize_path_adds_leading_slash() {
        assert_eq!(normalize_path("foo/bar"), "/foo/bar");
        assert_eq!(normalize_path("/foo"), "/foo");
        assert_eq!(normalize_path(""), "/");
    }

    #[test]
    fn test_auth_bearer_resolved() {
        let auth = AuthConfig::Bearer {
            token: "${steps.getToken}".into(),
        };
        let (k, v) = auth.auth_header_resolved("tok-abc").unwrap();
        assert_eq!(k, "authorization");
        assert_eq!(v, "Bearer tok-abc");
    }

    #[test]
    fn test_auth_basic_encodes() {
        let auth = AuthConfig::Basic {
            username: "user".into(),
            password: "pass".into(),
        };
        let (k, v) = auth.auth_header_resolved("").unwrap();
        assert_eq!(k, "authorization");
        assert!(v.starts_with("Basic "));
    }

    #[test]
    fn test_auth_passthrough_returns_none() {
        assert_eq!(AuthConfig::Passthrough.auth_header_resolved(""), None);
    }
}
