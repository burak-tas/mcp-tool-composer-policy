//! MCP Tool Composer Policy — entrypoint.
//!
//! Request flow for tools/call:
//!   1. inputTransform (DataWeave) reshapes MCP args → pipeline args
//!   2. Pipeline executes stages (sequential + parallel REST calls)
//!   3. outputTransform (DataWeave) shapes the composite result → MCP response

mod config;
mod dw;
mod generated;
mod jsonrpc;
mod pipeline;
mod schema;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::rc::Rc;

use pdk::hl::*;
use pdk::logger;
use serde_json::{json, Value};

use crate::config::PolicyConfig;
use crate::generated::config::Config;
use crate::jsonrpc::{
    error_response, success_response, tool_error_response, JsonRpcOutbound, JsonRpcRequest,
    INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
use crate::pipeline::{PipelineError, ServiceMap};

const POLICY_NAME: &str = "mcp-tool-composer";
const CONTENT_TYPE_HEADER: &str = "content-type";
const CONTENT_LENGTH_HEADER: &str = "content-length";
const APPLICATION_JSON: &str = "application/json";

// MCP protocol versions this policy implements, newest first (Streamable-HTTP
// transport). The first entry is the preferred version returned when
// negotiation finds no match.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Version assumed for a non-`initialize` request that omits the
/// `MCP-Protocol-Version` header, per the Streamable-HTTP transport spec.
const DEFAULT_NEGOTIATED_VERSION: &str = "2025-03-26";

/// The preferred (latest) protocol version — returned when the client requests
/// an unsupported version or none at all.
fn preferred_protocol_version() -> &'static str {
    SUPPORTED_PROTOCOL_VERSIONS[0]
}

fn is_supported_version(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

/// Negotiate the response `protocolVersion` for an `initialize` request: echo
/// the client's requested version when supported, otherwise fall back to the
/// preferred version (spec: respond with a version the server supports, never
/// the client's unsupported one).
fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(v) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .copied()
            .find(|s| *s == v)
            .unwrap_or_else(preferred_protocol_version),
        None => preferred_protocol_version(),
    }
}

/// Does an `Accept` header value accept `application/json`? Tokenizes the
/// comma-separated media ranges, ignores parameters (including `q` weights),
/// and honors the `*/*` and `application/*` wildcards — so a value like
/// `application/json-patch+json` never false-matches `application/json`.
fn accepts_json(accept: &str) -> bool {
    accept.split(',').any(|range| {
        let media = range
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        media == "*/*" || media == "application/*" || media == "application/json"
    })
}

/// Is `origin` permitted by the configured allowlist? `"*"` allows any origin;
/// otherwise the match is exact (case-insensitive on the scheme/host).
fn origin_allowed(origin: &str, allowlist: &[String]) -> bool {
    allowlist
        .iter()
        .any(|allowed| allowed == "*" || allowed.eq_ignore_ascii_case(origin))
}

// ---------------------------------------------------------------------------
// Request filter
// ---------------------------------------------------------------------------

async fn request_filter(
    request: RequestState,
    policy: Rc<PolicyConfig>,
    raw_config: Rc<Config>,
    service_map: Rc<ServiceMap>,
    client: HttpClient,
) -> Flow<()> {
    // This is a TERMINATING policy — it answers tools/call itself via
    // Flow::Break. Buffer request headers + body ATOMICALLY in one transition
    // (#15): the sequential into_headers_state().await → into_body_state().await
    // pattern releases the headers to Envoy's router on the first await, which
    // begins proxying upstream in parallel, so a synthetic Flow::Break can lose
    // the race to an upstream response. into_headers_body_state() holds the
    // request in the filter (never forwarded) so our response always wins. It
    // requires the `enable_stop_iteration` feature + a runtime with the
    // `flex_enable_stop_iteration` ABI (Flex/Omni Gateway >= 1.12.0).
    let state = request.into_headers_body_state().await;
    let handler = state.handler();

    // The combined headers+body state exposes only pseudo-headers (no
    // .method()/.path() convenience accessors), so read them directly.
    let path = handler.header(":path").unwrap_or_else(|| "/".to_string());
    let method = handler
        .header(":method")
        .unwrap_or_default()
        .to_ascii_uppercase();

    let bare = path.split_once('?').map(|(p, _)| p).unwrap_or(&path);
    if !bare.starts_with(&policy.mcp_endpoint) {
        if policy.strict_mode {
            return send_error(404, "Not Found");
        }
        return Flow::Continue(());
    }

    // Only POST carries MCP JSON-RPC messages. This server returns tool results
    // synchronously on the POST response and offers no server-initiated SSE
    // stream, so a GET / DELETE / etc. is 405 Method Not Allowed — the
    // Streamable-HTTP spec's sanctioned response for a server that does not
    // offer an SSE channel — never a stub event-stream that closes immediately.
    if method != "POST" {
        return method_not_allowed();
    }

    // Origin validation (DNS-rebinding protection). Only enforced when an
    // allowlist is configured; a request with no Origin header (a non-browser
    // client) is allowed, since the rebinding threat is browser-only.
    if !policy.allowed_origins.is_empty() {
        if let Some(origin) = handler.header("origin") {
            if !origin_allowed(&origin, &policy.allowed_origins) {
                return send_error(403, "Origin not allowed");
            }
        }
    }

    // Validate Content-Type for POST — must be application/json.
    let content_type = handler.header("content-type").unwrap_or_default();
    if !content_type.contains("application/json") {
        return send_json_rpc(
            200,
            &error_response(
                None,
                INVALID_REQUEST,
                "Content-Type must be application/json",
            ),
        );
    }

    // Accept, when present, must accept application/json — the only media type
    // this server emits. An absent Accept is tolerated (many non-browser MCP
    // clients omit it); a present-but-incompatible Accept is a 400 Bad Request.
    if let Some(accept) = handler.header("accept") {
        if !accepts_json(&accept) {
            return send_json_rpc(
                400,
                &error_response(
                    None,
                    INVALID_REQUEST,
                    "Accept must include application/json",
                ),
            );
        }
    }

    // Headers needed after the body is read:
    // - Authorization (forwarded for passthrough auth calls)
    // - MCP-Protocol-Version (validated post-parse, since `initialize` is exempt)
    let incoming_auth: Option<String> = handler.header("authorization");
    let requested_protocol_version: Option<String> = handler.header("mcp-protocol-version");

    // The body was buffered atomically above. `contains_body()` is false for a
    // bodyless request (shouldn't reach here — only POST gets this far — but be
    // defensive rather than panic).
    let body = if state.contains_body() {
        handler.body()
    } else {
        Vec::new()
    };

    // Payload-size cap (#16): reject an oversized request body before parsing so
    // the buffered request can't drive unbounded downstream work. The atomic
    // buffer itself is physically bounded by FLEX_DOWNSTREAM_CONNECTION_BUFFER_
    // LIMIT_BYTES (default 1 MiB); this is the explicit, configurable cap on top.
    if body.len() > policy.max_request_bytes {
        return send_json_rpc(
            200,
            &error_response(
                None,
                INVALID_REQUEST,
                format!(
                    "request body of {} bytes exceeds limit of {} bytes",
                    body.len(),
                    policy.max_request_bytes
                ),
            ),
        );
    }

    let rpc: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return send_json_rpc(
                200,
                &error_response(None, PARSE_ERROR, format!("Parse error: {e}")),
            );
        }
    };

    // Validate jsonrpc version and method presence BEFORE treating id-less
    // messages as notifications — a malformed envelope is always an error.
    if rpc.jsonrpc.as_deref() != Some("2.0") {
        return send_json_rpc(
            200,
            &error_response(rpc.id, INVALID_REQUEST, r#"jsonrpc must be "2.0""#),
        );
    }

    let method_name = match &rpc.method {
        Some(m) => m.clone(),
        None => {
            return send_json_rpc(
                200,
                &error_response(rpc.id, INVALID_REQUEST, "missing method"),
            );
        }
    };

    // MCP-Protocol-Version header (Streamable-HTTP): required on every request
    // AFTER initialization. `initialize` is exempt — the client cannot know the
    // negotiated version yet. A present-but-unsupported version is a 400 Bad
    // Request (spec MUST); an absent header falls back to the spec default
    // (`DEFAULT_NEGOTIATED_VERSION`) rather than erroring.
    if method_name != "initialize" {
        match requested_protocol_version.as_deref() {
            Some(pv) if !is_supported_version(pv) => {
                return send_json_rpc(
                    400,
                    &error_response(
                        rpc.id.clone(),
                        INVALID_REQUEST,
                        format!("unsupported MCP-Protocol-Version '{pv}'"),
                    ),
                );
            }
            Some(_) => {}
            None => {
                logger::debug!(
                    "[{}] no MCP-Protocol-Version header; assuming {}",
                    POLICY_NAME,
                    DEFAULT_NEGOTIATED_VERSION
                );
            }
        }
    }

    // After envelope validation: id-less messages are notifications — 202, empty body.
    if rpc.is_notification() {
        return send_raw(202, &[(CONTENT_TYPE_HEADER, APPLICATION_JSON)], b"");
    }

    match method_name.as_str() {
        "initialize" => handle_initialize(rpc.id, rpc.params.as_ref()),

        "ping" => send_json_rpc(200, &success_response(rpc.id, json!({}))),

        "notifications/initialized" | "notifications/cancelled" => {
            send_raw(202, &[(CONTENT_TYPE_HEADER, APPLICATION_JSON)], b"")
        }

        "tools/list" => handle_tools_list(&policy, rpc.id),

        "tools/call" => {
            handle_tools_call(
                &policy,
                &raw_config,
                &service_map,
                &client,
                rpc.id,
                rpc.params,
                incoming_auth.as_deref(),
            )
            .await
        }

        other => send_json_rpc(
            200,
            &error_response(
                rpc.id,
                METHOD_NOT_FOUND,
                format!("Method not supported: {other}"),
            ),
        ),
    }
}

// ---------------------------------------------------------------------------
// MCP handlers
// ---------------------------------------------------------------------------

fn handle_initialize(id: Option<Value>, params: Option<&Value>) -> Flow<()> {
    // Negotiate the protocol version from the client's request: echo it when
    // supported, otherwise return our preferred (latest) version and let the
    // client decide whether to disconnect.
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    let negotiated = negotiate_protocol_version(requested);

    send_json_rpc(
        200,
        &success_response(
            id,
            json!({
                "protocolVersion": negotiated,
                "serverInfo": {
                    "name": POLICY_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "capabilities": {
                    "tools": { "listChanged": false },
                }
            }),
        ),
    )
}

fn handle_tools_list(policy: &PolicyConfig, id: Option<Value>) -> Flow<()> {
    let tool = json!({
        "name": policy.tool_name,
        "description": policy.tool_description,
        "inputSchema": policy.tool_input_schema,
    });
    send_json_rpc(200, &success_response(id, json!({ "tools": [tool] })))
}

async fn handle_tools_call(
    policy: &PolicyConfig,
    raw_config: &Config,
    service_map: &ServiceMap,
    client: &HttpClient,
    id: Option<Value>,
    params: Option<Value>,
    incoming_auth: Option<&str>,
) -> Flow<()> {
    let params = params.unwrap_or(json!({}));

    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name != policy.tool_name {
        return send_json_rpc(
            200,
            &error_response(
                id,
                INVALID_PARAMS,
                format!("Unknown tool '{}'. Available: '{}'", name, policy.tool_name),
            ),
        );
    }

    // arguments must be an object when present.
    let raw_args = match params.get("arguments") {
        Some(Value::Object(_)) | None => params.get("arguments").cloned().unwrap_or(json!({})),
        Some(other) => {
            return send_json_rpc(
                200,
                &error_response(
                    id,
                    INVALID_PARAMS,
                    format!("'arguments' must be a JSON object, got {}", other),
                ),
            );
        }
    };

    // Validate arguments against the full toolInputSchema (types, enums,
    // bounds, required, additionalProperties) BEFORE running any transform or
    // dispatching the pipeline (#12). The error is sanitized — it names the
    // failing path and constraint, never the offending value.
    if let Err(err_msg) = schema::validate(&raw_args, &policy.tool_input_schema) {
        return send_json_rpc(
            200,
            &error_response(id, INVALID_PARAMS, format!("invalid arguments — {err_msg}")),
        );
    }

    // 1. Apply inputTransform — reshape args before pipeline.
    let pipeline_args = match dw::eval_transform(raw_config.input_transform.as_ref(), &raw_args) {
        Ok(v) => v,
        Err(e) => {
            // The DataWeave error `e` can echo the transform's input payload —
            // keep the detail server-side and return a generic message so
            // caller input (or credentials in it) never leaks (#13).
            logger::error!("[{}] inputTransform failed: {}", POLICY_NAME, e);
            return send_json_rpc(
                200,
                &tool_error_response(id, "inputTransform failed", "transform_error"),
            );
        }
    };

    // 2. Execute pipeline.
    let pipeline_result =
        match pipeline::run_pipeline(policy, service_map, &pipeline_args, client, incoming_auth)
            .await
        {
            Ok(r) => r,
            // Downstream execution failures → CallToolResult with isError:true so
            // MCP clients and models can inspect and recover.
            Err(PipelineError::HttpStatus { call, status }) => {
                return send_json_rpc(
                    200,
                    &tool_error_response(
                        id,
                        format!("call '{}' returned HTTP {}", call, status),
                        "http_error",
                    ),
                );
            }
            Err(PipelineError::Transport { call, message }) => {
                logger::error!(
                    "[{}] transport error on '{}': {}",
                    POLICY_NAME,
                    call,
                    message
                );
                return send_json_rpc(
                    200,
                    &tool_error_response(
                        id,
                        format!("call '{}' transport error", call),
                        "transport_error",
                    ),
                );
            }
            Err(PipelineError::BadJson { call, .. }) => {
                return send_json_rpc(
                    200,
                    &tool_error_response(
                        id,
                        format!("call '{}' returned non-JSON response", call),
                        "parse_error",
                    ),
                );
            }
            // Payload-size cap (#16): a downstream response exceeding the
            // configured limit fails the call. The message names the limit,
            // never the (potentially sensitive) oversized body.
            Err(PipelineError::ResponseTooLarge { call, limit, .. }) => {
                return send_json_rpc(
                    200,
                    &tool_error_response(
                        id,
                        format!("call '{}' response exceeded {} bytes", call, limit),
                        "response_too_large",
                    ),
                );
            }
            // Injection-safe construction (#11): an unresolved/malformed expression
            // aborts the call instead of sending a request with a hole in it. The
            // message carries only the expression text, never a resolved value.
            Err(PipelineError::Interpolation { call, message }) => {
                return send_json_rpc(
                    200,
                    &tool_error_response(
                        id,
                        format!("call '{}' could not be built: {}", call, message),
                        "invalid_argument",
                    ),
                );
            }
            Err(PipelineError::Timeout { call }) => {
                return send_json_rpc(
                    200,
                    &tool_error_response(id, format!("call '{}' timed out", call), "timeout"),
                );
            }
            Err(PipelineError::GlobalTimeout) => {
                return send_json_rpc(
                    200,
                    &tool_error_response(id, "pipeline exceeded global deadline", "timeout"),
                );
            }
        };

    // 3. Apply outputTransform — shape the composite result for the MCP client.
    let final_value = match dw::eval_transform(
        raw_config.output_transform.as_ref(),
        &pipeline_result.final_output,
    ) {
        Ok(v) => v,
        Err(e) => {
            // The DataWeave error `e` can echo the composite pipeline output,
            // which may carry un-masked ${steps.*} credentials — keep the
            // detail server-side and return a generic message (#13).
            logger::error!("[{}] outputTransform failed: {}", POLICY_NAME, e);
            return send_json_rpc(
                200,
                &tool_error_response(id, "outputTransform failed", "transform_error"),
            );
        }
    };

    // Payload-size cap (#16): bound the final serialized result so a pipeline
    // that composes many/large responses can't emit an unbounded MCP result.
    let final_text = final_value.to_string();
    if final_text.len() > policy.max_result_bytes {
        return send_json_rpc(
            200,
            &tool_error_response(
                id,
                format!(
                    "result exceeded {} bytes ({} bytes produced)",
                    policy.max_result_bytes,
                    final_text.len()
                ),
                "result_too_large",
            ),
        );
    }

    let content = json!([{
        "type": "text",
        "text": final_text,
    }]);
    send_json_rpc(
        200,
        &success_response(id, json!({ "content": content, "isError": false })),
    )
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn send_json_rpc(status: u32, payload: &JsonRpcOutbound) -> Flow<()> {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    send_raw(status, &[(CONTENT_TYPE_HEADER, APPLICATION_JSON)], &body)
}

/// 405 Method Not Allowed with an `Allow: POST` header — the Streamable-HTTP
/// response for any non-POST method, since this server offers no GET SSE
/// channel or session-termination DELETE.
fn method_not_allowed() -> Flow<()> {
    send_raw(
        405,
        &[(CONTENT_TYPE_HEADER, APPLICATION_JSON), ("allow", "POST")],
        br#"{"error":"Use POST for MCP requests"}"#,
    )
}

fn send_error(status: u32, detail: &str) -> Flow<()> {
    let body = format!(r#"{{"error":"{}"}}"#, detail);
    send_raw(
        status,
        &[(CONTENT_TYPE_HEADER, APPLICATION_JSON)],
        body.as_bytes(),
    )
}

fn send_raw(status: u32, headers: &[(&str, &str)], body: &[u8]) -> Flow<()> {
    let mut owned: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    owned.push((CONTENT_LENGTH_HEADER.to_string(), body.len().to_string()));
    Flow::Break(Response::new(status).with_headers(owned).with_body(body))
}

// ---------------------------------------------------------------------------
// PDK entrypoint
// ---------------------------------------------------------------------------

#[entrypoint]
pub async fn configure(
    launcher: Launcher,
    Configuration(bytes): Configuration,
) -> anyhow::Result<()> {
    let raw: Config = serde_json::from_slice(&bytes)
        .map_err(|e| anyhow::anyhow!("invalid policy configuration: {e}"))?;

    let policy = PolicyConfig::from_config(&raw)
        .map_err(|e| anyhow::anyhow!("policy configuration rejected: {e}"))?;

    let masked: Vec<&str> = policy
        .all_calls()
        .filter(|c| c.mask_in_output)
        .map(|c| c.name.as_str())
        .collect();
    logger::info!(
        "[{}] loaded '{}'; {} stage(s), {} call(s); endpoint='{}'; masked=[{}]; deadline={}ms",
        POLICY_NAME,
        policy.tool_name,
        policy.stages.len(),
        policy.all_calls().count(),
        policy.mcp_endpoint,
        masked.join(", "),
        policy.pipeline_timeout_ms,
    );

    // Build service map: call name → registered Flex Gateway Service handle.
    let mut service_map: ServiceMap = HashMap::new();
    for (stage_raw, stage_typed) in raw.stages.iter().zip(policy.stages.iter()) {
        for (call_raw, call_typed) in stage_raw.calls.iter().zip(stage_typed.calls().iter()) {
            service_map.insert(call_typed.name.clone(), Rc::new(call_raw.endpoint.clone()));
        }
    }

    let service_map = Rc::new(service_map);
    let policy = Rc::new(policy);
    let raw_config = Rc::new(raw);

    let filter = on_request(move |request: RequestState, client: HttpClient| {
        let policy = policy.clone();
        let raw_config = raw_config.clone();
        let service_map = service_map.clone();
        async move { request_filter(request, policy, raw_config, service_map, client).await }
    });

    launcher.launch(filter).await?;
    Ok(())
}
