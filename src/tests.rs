//! Entrypoint-level unit tests driving the real `configure` entrypoint through
//! `pdk-unit`. These exercise the MCP Streamable-HTTP transport, JSON-RPC
//! envelope validation, and method dispatch end-to-end — the surface the pure
//! helper tests in `config.rs` / `pipeline/expr.rs` never touch.
//!
//! This first batch covers every response the policy produces **locally**,
//! i.e. without dispatching the pipeline to an upstream service (initialize,
//! tools/list, ping, notifications, malformed input, unknown method, transport
//! guards, and argument validation). Pipeline tests that mock outbound HTTP
//! live in later batches.

#[cfg(test)]
mod tests {
    use pdk_unit::{UnitHttpRequest, UnitTestBuilder};
    use serde_json::{json, Value};

    const ENDPOINT: &str = "/mcp";
    const TOOL_NAME: &str = "createOrder";

    /// A minimal, valid policy configuration: one sequential stage with a single
    /// call to a stub upstream, a `customerId`-required input schema, and no
    /// DataWeave transforms (omitted → identity, see `dw::eval_transform`).
    fn default_config() -> String {
        json!({
            "mcpEndpoint": ENDPOINT,
            "strictMode": true,
            "toolName": TOOL_NAME,
            "toolDescription": "Creates an order via a composed REST pipeline.",
            "toolInputSchema": r#"{"type":"object","properties":{"customerId":{"type":"string"}},"required":["customerId"]}"#,
            "stages": [
                {
                    "calls": [
                        {
                            "name": "createOrder",
                            "endpoint": "https://orders.example.com",
                            "method": "POST",
                            "path": "/orders"
                        }
                    ]
                }
            ]
        })
        .to_string()
    }

    /// Build a tester over `configure` with the given config.
    fn tester(config: &str) -> pdk_unit::UnitTest {
        UnitTestBuilder::default()
            .with_config(config)
            .with_entrypoint(crate::configure)
    }

    /// Issue a JSON-RPC POST to the MCP endpoint and return the response.
    fn post_rpc(config: &str, rpc: Value) -> pdk_unit::UnitHttpResponse {
        tester(config).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_body(rpc.to_string()),
        )
    }

    /// Parse a JSON-RPC response body into a `serde_json::Value`.
    fn body_json(response: &pdk_unit::UnitHttpResponse) -> Value {
        use pdk_unit::UnitHttpMessage;
        serde_json::from_slice(response.body()).expect("response body must be valid JSON")
    }

    // -----------------------------------------------------------------------
    // initialize
    // -----------------------------------------------------------------------

    #[test]
    fn initialize_returns_protocol_version_and_server_info() {
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }),
        );
        assert_eq!(response.status_code(), 200);

        let body = body_json(&response);
        assert_eq!(body["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(body["result"]["serverInfo"]["name"], "mcp-tool-composer");
        assert_eq!(
            body["result"]["capabilities"]["tools"]["listChanged"],
            false
        );
        assert_eq!(body["id"], 1);
    }

    // -----------------------------------------------------------------------
    // tools/list
    // -----------------------------------------------------------------------

    #[test]
    fn tools_list_exposes_the_single_configured_tool() {
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        );
        assert_eq!(response.status_code(), 200);

        let body = body_json(&response);
        let tools = body["result"]["tools"]
            .as_array()
            .expect("result.tools must be an array");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], TOOL_NAME);
        // inputSchema is surfaced as a parsed object, and carries `required`.
        assert_eq!(tools[0]["inputSchema"]["type"], "object");
        assert_eq!(tools[0]["inputSchema"]["required"][0], "customerId");
    }

    // -----------------------------------------------------------------------
    // ping
    // -----------------------------------------------------------------------

    #[test]
    fn ping_returns_empty_result() {
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }),
        );
        assert_eq!(response.status_code(), 200);

        let body = body_json(&response);
        assert_eq!(body["result"], json!({}));
        assert!(body.get("error").is_none());
    }

    // -----------------------------------------------------------------------
    // notifications (id-less messages)
    // -----------------------------------------------------------------------

    #[test]
    fn known_notification_is_accepted_with_202() {
        use pdk_unit::UnitHttpMessage;
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        );
        assert_eq!(response.status_code(), 202);
        assert!(
            response.body().is_empty(),
            "notifications return an empty body"
        );
    }

    #[test]
    fn id_less_message_is_treated_as_notification_after_envelope_validation() {
        // A valid envelope with no id and an arbitrary method → 202 (notification),
        // never a method-not-found error.
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "2.0", "method": "tools/list" }),
        );
        assert_eq!(response.status_code(), 202);
    }

    // -----------------------------------------------------------------------
    // envelope validation
    // -----------------------------------------------------------------------

    #[test]
    fn malformed_json_body_is_a_parse_error() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_body("{ not valid json "),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32700);
    }

    #[test]
    fn wrong_jsonrpc_version_is_invalid_request() {
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "1.0", "id": 9, "method": "ping" }),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32600);
    }

    #[test]
    fn missing_method_is_invalid_request() {
        let response = post_rpc(&default_config(), json!({ "jsonrpc": "2.0", "id": 10 }));
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32600);
    }

    #[test]
    fn unknown_method_is_method_not_found() {
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "2.0", "id": 11, "method": "resources/list" }),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32601);
    }

    // -----------------------------------------------------------------------
    // transport guards
    // -----------------------------------------------------------------------

    #[test]
    fn get_with_sse_accept_opens_event_stream() {
        use pdk_unit::UnitHttpMessage;
        let response = tester(&default_config()).request(
            UnitHttpRequest::get()
                .with_path(ENDPOINT)
                .with_header("accept", "text/event-stream"),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(response.header("content-type"), Some("text/event-stream"));
    }

    #[test]
    fn get_without_sse_accept_is_405() {
        let response =
            tester(&default_config()).request(UnitHttpRequest::get().with_path(ENDPOINT));
        assert_eq!(response.status_code(), 405);
    }

    #[test]
    fn post_with_wrong_content_type_is_invalid_request() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "text/plain")
                .with_body(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string()),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32600);
    }

    #[test]
    fn non_mcp_path_is_404_in_strict_mode() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path("/not-mcp")
                .with_header("content-type", "application/json")
                .with_body(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string()),
        );
        assert_eq!(response.status_code(), 404);
    }

    // -----------------------------------------------------------------------
    // tools/call argument validation (rejected before the pipeline runs)
    // -----------------------------------------------------------------------

    #[test]
    fn tools_call_unknown_tool_name_is_invalid_params() {
        let response = post_rpc(
            &default_config(),
            json!({
                "jsonrpc": "2.0",
                "id": 20,
                "method": "tools/call",
                "params": { "name": "not-the-tool", "arguments": { "customerId": "c1" } }
            }),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32602);
    }

    #[test]
    fn tools_call_missing_required_argument_is_invalid_params() {
        let response = post_rpc(
            &default_config(),
            json!({
                "jsonrpc": "2.0",
                "id": 21,
                "method": "tools/call",
                "params": { "name": TOOL_NAME, "arguments": {} }
            }),
        );
        assert_eq!(response.status_code(), 200);
        let body = body_json(&response);
        assert_eq!(body["error"]["code"], -32602);
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("customerId"),
            "error should name the missing argument, got {body:?}"
        );
    }

    #[test]
    fn tools_call_non_object_arguments_is_invalid_params() {
        let response = post_rpc(
            &default_config(),
            json!({
                "jsonrpc": "2.0",
                "id": 22,
                "method": "tools/call",
                "params": { "name": TOOL_NAME, "arguments": "not-an-object" }
            }),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32602);
    }
}
