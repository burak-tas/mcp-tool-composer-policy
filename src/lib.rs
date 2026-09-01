//! MCP Tool Composer Policy — entrypoint.
//!
//! Exposes a single MCP tool via Flex Gateway's Streamable HTTP transport.
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

use std::collections::HashMap;
use std::rc::Rc;

use pdk::hl::*;
use pdk::logger;
use serde_json::{json, Value};

use crate::config::PolicyConfig;
use crate::generated::config::Config;
use crate::jsonrpc::{
    error_response, error_response_with_data, success_response, JsonRpcOutbound, JsonRpcRequest,
    INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
};
use crate::pipeline::{PipelineError, ServiceMap};

const POLICY_NAME: &str = "mcp-tool-composer";
const CONTENT_TYPE_HEADER: &str = "content-type";
const CONTENT_LENGTH_HEADER: &str = "content-length";
const APPLICATION_JSON: &str = "application/json";

// ---------------------------------------------------------------------------
// Request filter
// ---------------------------------------------------------------------------

async fn request_filter(
    request: RequestHeadersState,
    policy: Rc<PolicyConfig>,
    raw_config: Rc<Config>,
    service_map: Rc<ServiceMap>,
    client: HttpClient,
) -> Flow<()> {
    let path = request.path();
    let method = request.method().to_ascii_uppercase();

    let bare = path.split_once('?').map(|(p, _)| p).unwrap_or(&path);
    if !bare.starts_with(&policy.mcp_endpoint) {
        if policy.strict_mode {
            return send_error(404, "Not Found");
        }
        return Flow::Continue(());
    }

    // SSE keep-alive for GET requests.
    if method == "GET" {
        let accept = request.handler().header("accept").unwrap_or_default();
        if accept.contains("text/event-stream") {
            return send_raw(
                200,
                &[
                    (CONTENT_TYPE_HEADER, "text/event-stream"),
                    ("cache-control", "no-cache"),
                ],
                b"",
            );
        }
        return send_error(405, "Use POST for MCP requests");
    }

    if method != "POST" {
        return send_error(405, "Use POST for MCP requests");
    }

    // Capture incoming Authorization header for passthrough auth calls.
    let incoming_auth: Option<String> = request.handler().header("authorization");

    let body_state = request.into_body_state().await;
    let body = body_state.handler().body();

    let rpc: JsonRpcRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return send_json_rpc(
                200,
                &error_response(None, PARSE_ERROR, format!("Parse error: {e}")),
            );
        }
    };

    if rpc.is_notification() {
        return send_raw(202, &[(CONTENT_TYPE_HEADER, APPLICATION_JSON)], b"{}");
    }

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

    match method_name.as_str() {
        "initialize" => handle_initialize(rpc.id),

        "ping" => send_json_rpc(200, &success_response(rpc.id, json!({}))),

        "notifications/initialized" | "notifications/cancelled" => {
            send_raw(202, &[(CONTENT_TYPE_HEADER, APPLICATION_JSON)], b"{}")
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

fn handle_initialize(id: Option<Value>) -> Flow<()> {
    send_json_rpc(
        200,
        &success_response(
            id,
            json!({
                "protocolVersion": "2024-11-05",
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
                format!(
                    "Unknown tool '{}'. Available: '{}'",
                    name, policy.tool_name
                ),
            ),
        );
    }

    let raw_args = params.get("arguments").cloned().unwrap_or(json!({}));

    // 1. Apply inputTransform — reshape args before pipeline.
    let pipeline_args =
        dw::eval_transform(raw_config.input_transform.as_ref(), &raw_args);

    // 2. Execute pipeline.
    let pipeline_result = match pipeline::run_pipeline(
        policy,
        service_map,
        &pipeline_args,
        client,
        incoming_auth,
    )
    .await
    {
        Ok(r) => r,
        Err(PipelineError::HttpStatus { call, status }) => {
            return send_json_rpc(
                200,
                &error_response_with_data(
                    id,
                    INTERNAL_ERROR,
                    format!("Call '{call}' returned HTTP {status}"),
                    json!({ "call": call, "httpStatus": status }),
                ),
            )
        }
        Err(e) => {
            logger::error!("[{}] pipeline error: {}", POLICY_NAME, e);
            return send_json_rpc(200, &error_response(id, INTERNAL_ERROR, e.to_string()));
        }
    };

    // 3. Apply outputTransform — shape the composite result for the MCP client.
    let final_value =
        dw::eval_transform(raw_config.output_transform.as_ref(), &pipeline_result.final_output);

    let content = json!([{
        "type": "text",
        "text": final_value.to_string(),
    }]);
    send_json_rpc(200, &success_response(id, json!({ "content": content })))
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn send_json_rpc(status: u32, payload: &JsonRpcOutbound) -> Flow<()> {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    send_raw(status, &[(CONTENT_TYPE_HEADER, APPLICATION_JSON)], &body)
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

    logger::info!(
        "[{}] loaded '{}'; {} stage(s), {} call(s); endpoint='{}'",
        POLICY_NAME,
        policy.tool_name,
        policy.stages.len(),
        policy.all_calls().count(),
        policy.mcp_endpoint,
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

    let filter = on_request(move |request: RequestHeadersState, client: HttpClient| {
        let policy = policy.clone();
        let raw_config = raw_config.clone();
        let service_map = service_map.clone();
        async move {
            request_filter(request, policy, raw_config, service_map, client).await
        }
    });

    launcher.launch(filter).await?;
    Ok(())
}
