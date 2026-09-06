# Test Report — MCP Tool Composer Policy

**Date:** 2026-09-06 | **Flex Gateway:** 1.9.3 / 1.12.1 | **Policy:** v0.1.0
**Commit under test:** `21b757f` (merged PRs #20–#24)
**Unit tests:** `cargo test` — **88 / 88 passed** in 1.51 s
**Integration tests:** curl against live Flex Gateway — **14 / 14 passed**

---

## What changed in this revision

| PR | Title | Impact on tests |
|---|---|---|
| #20 | CI test harness | 49 new unit tests in `src/tests.rs` |
| #21 | Security hardening | Credential non-leak tests (#13); injection tests (#11) |
| #22 | MCP transport compliance (#14) | Protocol-version negotiation, Accept, Origin tests |
| #23 | Atomic buffering + payload limits (#15/#16) | `enable_stop_iteration` re-enabled (Flex ≥ 1.12.0); payload-size tests |
| #24 | Docs: transcoding comparison | README only, no test impact |

---

## Unit Tests — `cargo test` (88 / 88)

### Group 1 — MCP method dispatch (src/tests.rs)

| Test | What it verifies | Result |
|---|---|---|
| `initialize_returns_protocol_version_and_server_info` | `initialize` → 200, `protocolVersion=2025-06-18`, server name, capabilities | ✅ |
| `tools_list_exposes_the_single_configured_tool` | `tools/list` → 200, correct tool name + required array | ✅ |
| `ping_returns_empty_result` | `ping` → 200, `result={}`, no error | ✅ |
| `known_notification_is_accepted_with_202` | `notifications/initialized` (id-less) → 202, empty body | ✅ |
| `id_less_message_is_treated_as_notification_after_envelope_validation` | id-less valid envelope → 202, never method-not-found | ✅ |
| `unknown_method_is_method_not_found` | `resources/list` → 200, code `-32601` | ✅ |

### Group 2 — JSON-RPC envelope validation

| Test | What it verifies | Result |
|---|---|---|
| `malformed_json_body_is_a_parse_error` | `{ not valid json` → code `-32700` | ✅ |
| `wrong_jsonrpc_version_is_invalid_request` | `jsonrpc:"1.0"` → code `-32600` | ✅ |
| `missing_method_is_invalid_request` | body with no `method` field → code `-32600` | ✅ |

### Group 3 — Transport guards

| Test | What it verifies | Result |
|---|---|---|
| `get_with_sse_accept_is_405_not_a_stub_stream` | GET with `Accept: text/event-stream` → 405, `Allow: POST` header | ✅ |
| `get_without_sse_accept_is_405` | plain GET → 405 | ✅ |
| `delete_is_405` | DELETE → 405 | ✅ |
| `post_with_wrong_content_type_is_invalid_request` | `Content-Type: text/plain` → code `-32600` | ✅ |
| `non_mcp_path_is_404_in_strict_mode` | POST to `/not-mcp` with `strictMode:true` → 404 | ✅ |

### Group 4 — tools/call argument validation

| Test | What it verifies | Result |
|---|---|---|
| `tools_call_unknown_tool_name_is_invalid_params` | unknown tool name → code `-32602` | ✅ |
| `tools_call_missing_required_argument_is_invalid_params` | missing `customerId` → code `-32602`, field named in message | ✅ |
| `tools_call_non_object_arguments_is_invalid_params` | `arguments:"not-an-object"` → code `-32602` | ✅ |

### Group 5 — Injection safety (#11)

| Test | What it verifies | Result |
|---|---|---|
| `query_injection_is_percent_encoded_on_the_wire` | `city="Berlin&count=100"` → percent-encoded `Berlin%26count%3D100` on wire | ✅ |
| `body_injection_cannot_add_sibling_fields_on_the_wire` | quote-bearing `customerId` → stays in JSON string, no injected sibling key | ✅ |
| `unresolved_reference_fails_the_call_without_dispatching` | optional arg missing → `isError:true` + expression named, no upstream call | ✅ |

### Group 6 — Full toolInputSchema validation (#12)

| Test | What it verifies | Result |
|---|---|---|
| `schema_rejects_wrong_argument_type` | `customerId:123` (integer) → code `-32602`, field named, value NOT echoed | ✅ |
| `schema_rejects_out_of_range_number` | `quantity:999` (> max 100) → code `-32602` | ✅ |
| `schema_rejects_value_not_in_enum` | `tier:"bronze"` (not in `["gold","silver"]`) → code `-32602`, field named | ✅ |
| `schema_rejects_unexpected_argument` | `sneaky:true` with `additionalProperties:false` → code `-32602` | ✅ |

### Group 7 — Credential handling (#13)

| Test | What it verifies | Result |
|---|---|---|
| `static_bearer_token_is_sent_upstream_but_never_returned` | `Authorization: Bearer S3CRET-BEARER` reaches upstream; absent from response | ✅ |
| `basic_auth_password_is_encoded_upstream_and_never_returned` | plaintext password never on wire (Base64-encoded); absent from response | ✅ |
| `api_key_header_is_sent_upstream_but_never_returned` | `X-Api-Key: S3CRET-KEY` reaches upstream; absent from response | ✅ |
| `custom_header_credential_is_sent_upstream_but_never_returned` | custom auth header reaches upstream; absent from response | ✅ |
| `passthrough_forwards_incoming_authorization_but_never_returns_it` | inbound `Authorization` forwarded unchanged; not echoed in response | ✅ |
| `masked_step_derived_credential_is_not_echoed_in_response` | token from Stage 1 reaches Stage 2 upstream as `Bearer`; response shows `***` | ✅ |

### Group 8 — MCP Streamable-HTTP transport conformance (#14)

| Test | What it verifies | Result |
|---|---|---|
| `initialize_echoes_a_supported_requested_version` | `protocolVersion:"2025-03-26"` requested → echoed verbatim | ✅ |
| `initialize_negotiates_down_for_an_unsupported_version` | `protocolVersion:"1999-01-01"` → server replies `2025-06-18` | ✅ |
| `initialize_is_exempt_from_the_protocol_version_header` | bogus `MCP-Protocol-Version` header on initialize → still 200 | ✅ |
| `non_initialize_request_without_version_header_falls_back` | absent header → spec default assumed, no 400 | ✅ |
| `non_initialize_request_with_supported_version_header_ok` | `MCP-Protocol-Version: 2025-06-18` → 200 | ✅ |
| `non_initialize_request_with_unsupported_version_header_is_400` | `MCP-Protocol-Version: 1999-01-01` → 400, code `-32600` | ✅ |
| `accept_that_excludes_json_is_rejected` | `Accept: text/html` → 400, code `-32600` | ✅ |
| `accept_with_json_and_event_stream_is_ok` | `Accept: application/json, text/event-stream` → 200 | ✅ |
| `accept_wildcard_is_ok` | `Accept: */*` → 200 | ✅ |
| `disallowed_origin_is_403` | `Origin: https://evil.example` with allowlist → 403 | ✅ |
| `allowed_origin_passes` | `Origin: https://good.example` in allowlist → 200 | ✅ |
| `absent_origin_is_allowed_even_with_allowlist` | no Origin header → 200 (non-browser clients not blocked) | ✅ |
| `no_allowlist_skips_origin_validation` | no `allowedOrigins` config → any Origin passes | ✅ |

### Group 9 — Payload-size limits + no-rollback (#15/#16)

| Test | What it verifies | Result |
|---|---|---|
| `oversized_request_body_is_rejected_before_parsing` | body > `maxRequestBytes` → code `-32600` "exceeds limit", before parse | ✅ |
| `oversized_downstream_response_fails_the_call` | upstream response > `maxResponseBytes` → `isError:true`, `response_too_large` | ✅ |
| `oversized_final_result_fails_the_call` | result > `maxResultBytes` → `isError:true`, `result_too_large` | ✅ |
| `within_limits_call_succeeds` | all caps generous → `isError:false` | ✅ |
| `failure_after_earlier_mutating_stage_is_not_rolled_back` | stage 2 fails → `isError:true`; stage 1 already fired (no rollback) | ✅ |
| `global_pipeline_deadline_is_enforced` | stage 1 sleeps 1500 ms > `pipelineTimeoutMs:1000` → `isError:true`, "deadline" | ✅ |

### Schema unit tests (src/schema.rs — 39 tests)

All 39 tests from the JSON Schema validator pass (type checking, enum, bounds, `additionalProperties`, `required`, nested objects, `minLength`/`maxLength`, sanitized error messages that name fields but never echo user values).

---

## Integration Tests — Live Flex Gateway (14 / 14)

**Endpoint:** `https://mg-root-small-okoqaf.2wag5p.usa-e2.cloudhub.io/mcptoolcomp/mcp`
**Policy version deployed:** `0.1.0-20260906061959` (from commit `21b757f`)

### Pipeline under test (createOrder)

```
Stage 1 (sequential)   Auth Service    → none auth      → access_token (maskInOutput: true)
Stage 2 (parallel)     Customer API    → Bearer token   → customer profile
                       Inventory API   → X-API-Key      → stock level
Stage 3 (sequential)   Orders API      → Basic auth     → creates order
```

### Results

| # | Description | Expected | Actual | Result |
|---|---|---|---|---|
| TC-01 | `tools/list` | Tool `createOrder`, `required: [customerId, productSku, quantity]` | Correct schema returned | ✅ PASS |
| TC-02 | Happy path — CUST-001 + SKU-7842, qty 5 | All stages complete, `isError:false`, token masked | `getToken:"***"`, order confirmed | ✅ PASS |
| TC-03 | Unknown tool name | `-32602` with available tools listed | `"Unknown tool 'deleteEverything'"` | ✅ PASS |
| TC-04 | Missing `customerId` | `-32602` **before any network call** | `"missing required argument(s): customerId"` | ✅ PASS |
| TC-05 | Unknown customer (CUST-999) | `isError:true`, HTTP 404 detail | `[http_error] call 'fetchCustomer' returned HTTP 404` | ✅ PASS |
| TC-06 | Non-MCP path, `strictMode:true` | HTTP 404 | HTTP 404 | ✅ PASS |
| TC-07 | Out-of-stock SKU, `stopOnError:false` | Pipeline continues, token masked | Order confirmed, `available:false`, `getToken:"***"` | ✅ PASS |
| TC-08 | Malformed JSON body | `-32700 Parse error` | `"Parse error: key must be a string…"` | ✅ PASS |
| TC-09 | Unsupported method (`resources/list`) | `-32601 Method not found` | `"Method not supported: resources/list"` | ✅ PASS |
| TC-10 | GET without SSE Accept | HTTP 405 | HTTP 405 | ✅ PASS |
| TC-11 | `maskInOutput:true` — token masked, pipeline propagates | `getToken:"***"`, downstream call succeeds | Token masked, Bearer call succeeded, order confirmed | ✅ PASS |
| TC-12 | P4A → Anypoint deploy | Build pipeline + publish to Exchange | Published `0.1.0-20260906061959` ✅ | ✅ PASS |
| TC-13 | Wrong `Content-Type` | `-32600 Invalid Request` | `"Content-Type must be application/json"` | ✅ PASS |
| TC-14 | `arguments` not an object | `-32602 Invalid Params` | `"'arguments' must be a JSON object"` | ✅ PASS |

---

## Behaviour changes since previous report

| Area | Before (`2eb4347`) | After (`21b757f`) |
|---|---|---|
| Protocol version | Fixed `2024-11-05` | Negotiated — echoes client's version if supported, else `2025-06-18` |
| `MCP-Protocol-Version` header | Ignored | Validated on non-initialize requests; unsupported → 400 |
| `Accept` header | Ignored | `text/html`-only → 400; `*/*` and `application/json` → pass |
| Origin validation | Not supported | `allowedOrigins` config; disallowed → 403; absent Origin always passes |
| GET + SSE Accept | Returns 200 empty event-stream | Returns 405 with `Allow: POST` (spec-correct) |
| Input schema validation | `required` array only | Full JSON Schema: types, enums, bounds, `additionalProperties` |
| Injection safety | No encoding | Query params percent-encoded; body template values JSON-escaped |
| Payload size limits | Unbounded | `maxRequestBytes`, `maxResponseBytes`, `maxResultBytes` — all enforced |
| Unresolved `${args.*}` | Sent as literal string `"${args.x}"` | `isError:true` before dispatch, expression named in error |
| Atomic buffering | Race possible on early response | `enable_stop_iteration` (Flex ≥ 1.12.0) — headers+body buffered atomically |

---

## Exchange Assets (current)

| Asset ID | Version | Type |
|---|---|---|
| `mcp-tool-composer-policy-dev` | `0.1.0-20260906061959` | Definition |
| `mcp-tool-composer-policy-impl-dev` | `0.1.0-20260906061959` | Implementation |

---

## Known Limitations

| # | Limitation |
|---|---|
| L-1 | `outputTransform` / `inputTransform` DataWeave not supported on Flex 1.9.x (dw2pel restriction) — omit in production configs |
| L-2 | Completed stages are **not rolled back** on failure — design mutating pipelines with idempotency keys |
| L-3 | `enable_stop_iteration` requires Flex / Omni Gateway ≥ 1.12.0 — do not deploy to earlier runtimes |
| L-4 | One policy instance = one MCP tool; apply multiple times for multiple tools |
| L-5 | Output is text-only (`content[0].type:"text"`); `structuredContent` not supported |
