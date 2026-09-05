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
        // No requested version → server responds with its preferred (latest).
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
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
    fn get_with_sse_accept_is_405_not_a_stub_stream() {
        use pdk_unit::UnitHttpMessage;
        // This server offers no server-initiated SSE stream, so a GET — even one
        // asking for text/event-stream — is 405 (the spec-sanctioned response),
        // never a 200 empty event-stream that closes immediately (#14).
        let response = tester(&default_config()).request(
            UnitHttpRequest::get()
                .with_path(ENDPOINT)
                .with_header("accept", "text/event-stream"),
        );
        assert_eq!(response.status_code(), 405);
        assert_eq!(response.header("allow"), Some("POST"));
    }

    #[test]
    fn get_without_sse_accept_is_405() {
        let response =
            tester(&default_config()).request(UnitHttpRequest::get().with_path(ENDPOINT));
        assert_eq!(response.status_code(), 405);
    }

    #[test]
    fn delete_is_405() {
        let response =
            tester(&default_config()).request(UnitHttpRequest::delete().with_path(ENDPOINT));
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

    // -----------------------------------------------------------------------
    // Injection-safe construction (#11), end-to-end through the pipeline.
    //
    // These dispatch a real `tools/call` to a mocked upstream and assert the
    // *outbound* request the policy builds — the surface a canned-response mock
    // would never catch. The mock records the request it received.
    // -----------------------------------------------------------------------

    /// A config whose single call interpolates caller args into BOTH the URL
    /// query (`city`) and the JSON body (`customerId`), mirroring the real
    /// injection sites the issue calls out.
    fn injection_config() -> String {
        json!({
            "mcpEndpoint": ENDPOINT,
            "strictMode": true,
            "toolName": TOOL_NAME,
            "toolDescription": "Composed call that interpolates caller input.",
            "toolInputSchema": r#"{"type":"object","properties":{"customerId":{"type":"string"},"city":{"type":"string"}},"required":["customerId","city"]}"#,
            "stages": [
                {
                    "calls": [
                        {
                            "name": "createOrder",
                            "endpoint": "https://orders.example.com",
                            "method": "POST",
                            "path": "/v1/search?name=${args.city}&count=1",
                            "bodyTemplate": r#"{"customerId":"${args.customerId}"}"#
                        }
                    ]
                }
            ]
        })
        .to_string()
    }

    /// Drive a `tools/call` with the given arguments against `injection_config`,
    /// returning `(outbound_path, outbound_body, mcp_response)`.
    fn call_with_args(arguments: Value) -> (String, String, pdk_unit::UnitHttpResponse) {
        use pdk_unit::{UnitHttpMessage, UnitHttpRequest as MockReq, UnitHttpResponse};
        use std::cell::RefCell;
        use std::rc::Rc;

        let seen: Rc<RefCell<(String, String)>> =
            Rc::new(RefCell::new((String::new(), String::new())));
        let recorder = seen.clone();

        let response = UnitTestBuilder::default()
            .with_config(&injection_config())
            .with_http_upstream_from_authority("orders.example.com", move |req: MockReq| {
                // The outbound path is carried in the `:path` pseudo-header.
                let path = req.header(":path").unwrap_or_default().to_string();
                let body = String::from_utf8_lossy(req.body()).to_string();
                *recorder.borrow_mut() = (path, body);
                UnitHttpResponse::new(200).with_body(r#"{"ok":true}"#)
            })
            .with_entrypoint(crate::configure)
            .request(
                UnitHttpRequest::post()
                    .with_path(ENDPOINT)
                    .with_header("content-type", "application/json")
                    .with_body(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 30,
                            "method": "tools/call",
                            "params": { "name": TOOL_NAME, "arguments": arguments }
                        })
                        .to_string(),
                    ),
            );

        let (path, body) = seen.borrow().clone();
        (path, body, response)
    }

    #[test]
    fn query_injection_is_percent_encoded_on_the_wire() {
        // A `&`/`=`-bearing city must NOT inject a second query parameter.
        let (path, _body, response) = call_with_args(json!({
            "customerId": "CU-1",
            "city": "Berlin&count=100"
        }));
        assert_eq!(response.status_code(), 200);
        assert!(
            path.contains("name=Berlin%26count%3D100"),
            "city must be percent-encoded, got path {path:?}"
        );
        assert!(
            !path.contains("name=Berlin&count=100"),
            "injected raw query param must not reach the wire, got path {path:?}"
        );
    }

    #[test]
    fn body_injection_cannot_add_sibling_fields_on_the_wire() {
        // A quote-bearing customerId must stay contained in its JSON string.
        let (_path, body, response) = call_with_args(json!({
            "customerId": r#"","admin":true,"x":""#,
            "city": "Berlin"
        }));
        assert_eq!(response.status_code(), 200);
        let parsed: Value = serde_json::from_str(&body).expect("outbound body must be valid JSON");
        assert_eq!(parsed["customerId"], r#"","admin":true,"x":""#);
        assert!(
            parsed.get("admin").is_none(),
            "caller input must not inject a sibling field, got body {body:?}"
        );
    }

    #[test]
    fn unresolved_reference_fails_the_call_without_dispatching() {
        // A body template referencing an arg the schema does not require and the
        // caller omits → the call is rejected (isError:true), not sent with a
        // hole. `note` is optional here, so it passes arg-validation but fails
        // interpolation.
        let cfg = json!({
            "mcpEndpoint": ENDPOINT,
            "strictMode": true,
            "toolName": TOOL_NAME,
            "toolDescription": "Interpolates an optional, possibly-missing arg.",
            "toolInputSchema": r#"{"type":"object","properties":{"customerId":{"type":"string"},"note":{"type":"string"}},"required":["customerId"]}"#,
            "stages": [
                {
                    "calls": [
                        {
                            "name": "createOrder",
                            "endpoint": "https://orders.example.com",
                            "method": "POST",
                            "path": "/orders",
                            "bodyTemplate": r#"{"note":"${args.note}"}"#
                        }
                    ]
                }
            ]
        })
        .to_string();

        use pdk_unit::UnitHttpMessage;
        let response = tester(&cfg).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_body(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 31,
                        "method": "tools/call",
                        "params": { "name": TOOL_NAME, "arguments": { "customerId": "CU-1" } }
                    })
                    .to_string(),
                ),
        );
        assert_eq!(response.status_code(), 200);
        let body: Value = serde_json::from_slice(response.body()).unwrap();
        // CallToolResult with isError:true — the model can see and recover.
        assert_eq!(body["result"]["isError"], true);
        let text = body["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        assert!(
            text.contains("args.note"),
            "error should name the unresolved expression, got {text:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Full toolInputSchema validation (#12) — types/enums/bounds, not just
    // `required`, are enforced before the pipeline runs.
    // -----------------------------------------------------------------------

    /// Config whose schema types + constrains its arguments.
    fn typed_schema_config() -> String {
        json!({
            "mcpEndpoint": ENDPOINT,
            "strictMode": true,
            "toolName": TOOL_NAME,
            "toolDescription": "Typed-argument tool.",
            "toolInputSchema": r#"{"type":"object","properties":{"customerId":{"type":"string","minLength":1},"quantity":{"type":"integer","minimum":1,"maximum":100},"tier":{"type":"string","enum":["gold","silver"]}},"required":["customerId"],"additionalProperties":false}"#,
            "stages": [
                { "calls": [ { "name": "createOrder", "endpoint": "https://orders.example.com", "method": "POST", "path": "/orders" } ] }
            ]
        })
        .to_string()
    }

    fn call_typed(arguments: Value) -> pdk_unit::UnitHttpResponse {
        post_rpc(
            &typed_schema_config(),
            json!({
                "jsonrpc": "2.0",
                "id": 40,
                "method": "tools/call",
                "params": { "name": TOOL_NAME, "arguments": arguments }
            }),
        )
    }

    #[test]
    fn schema_rejects_wrong_argument_type() {
        let response = call_typed(json!({ "customerId": 123 }));
        assert_eq!(response.status_code(), 200);
        let body = body_json(&response);
        assert_eq!(body["error"]["code"], -32602);
        // Sanitized: names the field + type, never the offending value.
        let msg = body["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains("customerId"), "got: {msg}");
        assert!(!msg.contains("123"), "must not echo the value, got: {msg}");
    }

    #[test]
    fn schema_rejects_out_of_range_number() {
        let response = call_typed(json!({ "customerId": "c", "quantity": 999 }));
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32602);
    }

    #[test]
    fn schema_rejects_value_not_in_enum() {
        let response = call_typed(json!({ "customerId": "c", "tier": "bronze" }));
        assert_eq!(response.status_code(), 200);
        let msg = body_json(&response)["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(msg.contains("tier"), "got: {msg}");
    }

    #[test]
    fn schema_rejects_unexpected_argument() {
        let response = call_typed(json!({ "customerId": "c", "sneaky": true }));
        assert_eq!(response.status_code(), 200);
        assert_eq!(body_json(&response)["error"]["code"], -32602);
    }

    // -----------------------------------------------------------------------
    // Credential handling (#13) — sensitive material reaches the upstream but
    // is never surfaced back to the MCP client or in an error.
    //
    // The mock upstream echoes a fixed, secret-free body, so the ONLY way a
    // credential could appear in the MCP response is if the policy itself
    // leaked it. Each test asserts (a) the upstream received the expected auth
    // header (the credential is actually used) and (b) no credential value
    // appears anywhere in the MCP response envelope.
    // -----------------------------------------------------------------------

    /// Build a single-call config carrying the given auth-related fields merged
    /// onto the call, dispatch a `tools/call`, and return the headers the
    /// upstream saw plus the MCP response. `incoming_auth`, when set, is sent as
    /// the inbound `Authorization` header (exercises authType=passthrough).
    fn call_with_auth(
        auth_fields: Value,
        incoming_auth: Option<&str>,
    ) -> (
        std::collections::HashMap<String, String>,
        pdk_unit::UnitHttpResponse,
    ) {
        use pdk_unit::{UnitHttpMessage, UnitHttpRequest as MockReq, UnitHttpResponse};
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::rc::Rc;

        // Merge the auth fields onto a base call definition.
        let mut call = json!({
            "name": "createOrder",
            "endpoint": "https://orders.example.com",
            "method": "POST",
            "path": "/orders",
            "bodyTemplate": r#"{"customerId":"${args.customerId}"}"#
        });
        if let (Some(base), Some(extra)) = (call.as_object_mut(), auth_fields.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }

        let config = json!({
            "mcpEndpoint": ENDPOINT,
            "strictMode": true,
            "toolName": TOOL_NAME,
            "toolDescription": "Auth-bearing composed call.",
            "toolInputSchema": r#"{"type":"object","properties":{"customerId":{"type":"string"}},"required":["customerId"]}"#,
            "stages": [ { "calls": [ call ] } ]
        })
        .to_string();

        let seen: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let recorder = seen.clone();

        let mut req = UnitHttpRequest::post()
            .with_path(ENDPOINT)
            .with_header("content-type", "application/json");
        if let Some(auth) = incoming_auth {
            req = req.with_header("authorization", auth);
        }
        req = req.with_body(
            json!({
                "jsonrpc": "2.0",
                "id": 50,
                "method": "tools/call",
                "params": { "name": TOOL_NAME, "arguments": { "customerId": "CU-1" } }
            })
            .to_string(),
        );

        let response = UnitTestBuilder::default()
            .with_config(&config)
            .with_http_upstream_from_authority("orders.example.com", move |r: MockReq| {
                let mut map = recorder.borrow_mut();
                for (k, v) in r.headers() {
                    map.insert(k.to_ascii_lowercase(), v.to_string());
                }
                // A secret-free canned body — anything sensitive in the MCP
                // response must therefore have come from the policy leaking it.
                UnitHttpResponse::new(200).with_body(r#"{"ok":true}"#)
            })
            .with_entrypoint(crate::configure)
            .request(req);

        let headers = seen.borrow().clone();
        (headers, response)
    }

    /// The full MCP response envelope as a string, for leak assertions.
    fn response_text(response: &pdk_unit::UnitHttpResponse) -> String {
        use pdk_unit::UnitHttpMessage;
        String::from_utf8_lossy(response.body()).to_string()
    }

    #[test]
    fn static_bearer_token_is_sent_upstream_but_never_returned() {
        let (headers, response) = call_with_auth(
            json!({ "authType": "bearerToken", "token": "S3CRET-BEARER" }),
            None,
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer S3CRET-BEARER"),
            "upstream must receive the bearer token"
        );
        assert!(
            !response_text(&response).contains("S3CRET-BEARER"),
            "bearer token must not appear in the MCP response"
        );
    }

    #[test]
    fn basic_auth_password_is_encoded_upstream_and_never_returned() {
        let (headers, response) = call_with_auth(
            json!({ "authType": "basicAuth", "username": "svc", "password": "S3CRET-PASS" }),
            None,
        );
        assert_eq!(response.status_code(), 200);
        let auth = headers.get("authorization").cloned().unwrap_or_default();
        assert!(auth.starts_with("Basic "), "got {auth:?}");
        // The plaintext password must never appear — neither on the wire header
        // (it is Base64-encoded) nor in the MCP response.
        assert!(
            !auth.contains("S3CRET-PASS"),
            "plaintext password on the wire"
        );
        assert!(
            !response_text(&response).contains("S3CRET-PASS"),
            "password must not appear in the MCP response"
        );
    }

    #[test]
    fn api_key_header_is_sent_upstream_but_never_returned() {
        let (headers, response) = call_with_auth(
            json!({ "authType": "apiKeyHeader", "headerName": "x-api-key", "apiKey": "S3CRET-KEY" }),
            None,
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(
            headers.get("x-api-key").map(String::as_str),
            Some("S3CRET-KEY"),
            "upstream must receive the api key"
        );
        assert!(
            !response_text(&response).contains("S3CRET-KEY"),
            "api key must not appear in the MCP response"
        );
    }

    #[test]
    fn custom_header_credential_is_sent_upstream_but_never_returned() {
        let (headers, response) = call_with_auth(
            json!({
                "authType": "customHeaders",
                "headers": [ { "name": "x-secret", "value": "S3CRET-CUSTOM" } ]
            }),
            None,
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(
            headers.get("x-secret").map(String::as_str),
            Some("S3CRET-CUSTOM"),
            "upstream must receive the custom auth header"
        );
        assert!(
            !response_text(&response).contains("S3CRET-CUSTOM"),
            "custom-header credential must not appear in the MCP response"
        );
    }

    #[test]
    fn passthrough_forwards_incoming_authorization_but_never_returns_it() {
        let (headers, response) = call_with_auth(
            json!({ "authType": "passthrough" }),
            Some("Bearer CALLER-S3CRET"),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer CALLER-S3CRET"),
            "the incoming Authorization must be forwarded unchanged"
        );
        assert!(
            !response_text(&response).contains("CALLER-S3CRET"),
            "the forwarded credential must not be echoed back in the MCP response"
        );
    }

    #[test]
    fn masked_step_derived_credential_is_not_echoed_in_response() {
        // Stage 1 fetches a token (masked); stage 2 uses it as a bearer credential.
        // The token must reach the protected upstream but be "***" in the response.
        use pdk_unit::{UnitHttpMessage, UnitHttpRequest as MockReq, UnitHttpResponse};
        use std::cell::RefCell;
        use std::rc::Rc;

        let config = json!({
            "mcpEndpoint": ENDPOINT,
            "strictMode": true,
            "toolName": TOOL_NAME,
            "toolDescription": "Authenticate then call a protected endpoint.",
            "toolInputSchema": r#"{"type":"object","properties":{"customerId":{"type":"string"}},"required":["customerId"]}"#,
            "stages": [
                {
                    "calls": [ {
                        "name": "authenticate",
                        "endpoint": "https://auth.example.com",
                        "method": "POST",
                        "path": "/token",
                        "outputExtract": "token",
                        "maskInOutput": true
                    } ]
                },
                {
                    "calls": [ {
                        "name": "createOrder",
                        "endpoint": "https://orders.example.com",
                        "method": "POST",
                        "path": "/orders",
                        "authType": "bearerToken",
                        "token": "${steps.authenticate}"
                    } ]
                }
            ]
        })
        .to_string();

        let protected_auth: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let recorder = protected_auth.clone();

        let response = UnitTestBuilder::default()
            .with_config(&config)
            .with_http_upstream_from_authority("auth.example.com", |_r: MockReq| {
                UnitHttpResponse::new(200).with_body(r#"{"token":"STEP-S3CRET"}"#)
            })
            .with_http_upstream_from_authority("orders.example.com", move |r: MockReq| {
                *recorder.borrow_mut() = r.header("authorization").unwrap_or_default().to_string();
                UnitHttpResponse::new(200).with_body(r#"{"ok":true}"#)
            })
            .with_entrypoint(crate::configure)
            .request(
                UnitHttpRequest::post()
                    .with_path(ENDPOINT)
                    .with_header("content-type", "application/json")
                    .with_body(
                        json!({
                            "jsonrpc": "2.0",
                            "id": 51,
                            "method": "tools/call",
                            "params": { "name": TOOL_NAME, "arguments": { "customerId": "CU-1" } }
                        })
                        .to_string(),
                    ),
            );

        assert_eq!(response.status_code(), 200);
        // The step-derived token reached the protected upstream (auth works)...
        assert_eq!(
            *protected_auth.borrow(),
            "Bearer STEP-S3CRET",
            "the resolved step token must be used for the protected call"
        );
        // ...but the masked step's value is "***" in the response, never raw.
        let text = response_text(&response);
        assert!(
            !text.contains("STEP-S3CRET"),
            "masked step credential must not appear in the MCP response, got {text:?}"
        );
        assert!(
            text.contains("***"),
            "masked step output must render as ***, got {text:?}"
        );
    }

    // -----------------------------------------------------------------------
    // MCP Streamable-HTTP transport conformance (#14) — protocolVersion
    // negotiation, the MCP-Protocol-Version header, Accept, and Origin.
    // -----------------------------------------------------------------------

    #[test]
    fn initialize_echoes_a_supported_requested_version() {
        let response = post_rpc(
            &default_config(),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-03-26" }
            }),
        );
        assert_eq!(response.status_code(), 200);
        // Supported version → echoed verbatim.
        assert_eq!(
            body_json(&response)["result"]["protocolVersion"],
            "2025-03-26"
        );
    }

    #[test]
    fn initialize_negotiates_down_for_an_unsupported_version() {
        let response = post_rpc(
            &default_config(),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "1999-01-01" }
            }),
        );
        assert_eq!(response.status_code(), 200);
        let v = body_json(&response)["result"]["protocolVersion"].clone();
        // Unsupported → server responds with its preferred version, NOT the
        // client's requested one.
        assert_eq!(v, "2025-06-18");
        assert_ne!(v, "1999-01-01");
    }

    #[test]
    fn initialize_is_exempt_from_the_protocol_version_header() {
        // initialize carries no MCP-Protocol-Version header (the client cannot
        // know the version yet) — even a bogus one must not turn it into a 400.
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_header("mcp-protocol-version", "1999-01-01")
                .with_body(
                    json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize" }).to_string(),
                ),
        );
        assert_eq!(response.status_code(), 200);
        assert_eq!(
            body_json(&response)["result"]["protocolVersion"],
            "2025-06-18"
        );
    }

    #[test]
    fn non_initialize_request_without_version_header_falls_back() {
        // Absent MCP-Protocol-Version → assume the spec default, do NOT 400.
        let response = post_rpc(
            &default_config(),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        );
        assert_eq!(response.status_code(), 200);
        assert!(body_json(&response)["result"]["tools"].is_array());
    }

    #[test]
    fn non_initialize_request_with_supported_version_header_ok() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_header("mcp-protocol-version", "2025-06-18")
                .with_body(
                    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string(),
                ),
        );
        assert_eq!(response.status_code(), 200);
    }

    #[test]
    fn non_initialize_request_with_unsupported_version_header_is_400() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_header("mcp-protocol-version", "1999-01-01")
                .with_body(
                    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string(),
                ),
        );
        // Spec MUST: an unsupported MCP-Protocol-Version is a 400 Bad Request.
        assert_eq!(response.status_code(), 400);
        assert_eq!(body_json(&response)["error"]["code"], -32600);
    }

    #[test]
    fn accept_that_excludes_json_is_rejected() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_header("accept", "text/html")
                .with_body(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }).to_string()),
        );
        assert_eq!(response.status_code(), 400);
        assert_eq!(body_json(&response)["error"]["code"], -32600);
    }

    #[test]
    fn accept_with_json_and_event_stream_is_ok() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_header("accept", "application/json, text/event-stream")
                .with_body(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }).to_string()),
        );
        assert_eq!(response.status_code(), 200);
    }

    #[test]
    fn accept_wildcard_is_ok() {
        let response = tester(&default_config()).request(
            UnitHttpRequest::post()
                .with_path(ENDPOINT)
                .with_header("content-type", "application/json")
                .with_header("accept", "*/*")
                .with_body(json!({ "jsonrpc": "2.0", "id": 3, "method": "ping" }).to_string()),
        );
        assert_eq!(response.status_code(), 200);
    }

    /// A config identical to `default_config` plus an Origin allowlist.
    fn origin_restricted_config() -> String {
        json!({
            "mcpEndpoint": ENDPOINT,
            "strictMode": true,
            "allowedOrigins": ["https://good.example"],
            "toolName": TOOL_NAME,
            "toolDescription": "Origin-restricted MCP tool.",
            "toolInputSchema": r#"{"type":"object","properties":{"customerId":{"type":"string"}},"required":["customerId"]}"#,
            "stages": [
                { "calls": [ { "name": "createOrder", "endpoint": "https://orders.example.com", "method": "POST", "path": "/orders" } ] }
            ]
        })
        .to_string()
    }

    fn ping_with_origin(config: &str, origin: Option<&str>) -> pdk_unit::UnitHttpResponse {
        let mut req = UnitHttpRequest::post()
            .with_path(ENDPOINT)
            .with_header("content-type", "application/json");
        if let Some(o) = origin {
            req = req.with_header("origin", o);
        }
        req = req.with_body(json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }).to_string());
        tester(config).request(req)
    }

    #[test]
    fn disallowed_origin_is_403() {
        let response = ping_with_origin(&origin_restricted_config(), Some("https://evil.example"));
        assert_eq!(response.status_code(), 403);
    }

    #[test]
    fn allowed_origin_passes() {
        let response = ping_with_origin(&origin_restricted_config(), Some("https://good.example"));
        assert_eq!(response.status_code(), 200);
    }

    #[test]
    fn absent_origin_is_allowed_even_with_allowlist() {
        // A non-browser client sends no Origin — the rebinding threat is
        // browser-only, so it must pass.
        let response = ping_with_origin(&origin_restricted_config(), None);
        assert_eq!(response.status_code(), 200);
    }

    #[test]
    fn no_allowlist_skips_origin_validation() {
        // default_config has no allowedOrigins → any Origin is accepted.
        let response = ping_with_origin(&default_config(), Some("https://evil.example"));
        assert_eq!(response.status_code(), 200);
    }
}
