# Test Report — MCP Tool Composer Policy

**Date:** 2026-09-04 | **Flex Gateway:** 1.9.3 | **Policy:** v0.1.0
**Backend:** 4 A2D mock REST APIs | **Pipeline:** 3 stages, 4 calls
**Commit under test:** `2eb4347`

---

## Pipeline under test

```
Stage 1 (sequential)   Auth Service    → none auth      → extracts access_token (maskInOutput: true)
Stage 2 (parallel)     Customer API    → Bearer token   → returns customer profile
                       Inventory API   → X-API-Key      → returns stock level
Stage 3 (sequential)   Orders API      → Basic auth     → creates order
```

---

## TC-02 — Happy Path (all stages successful, token masked)

| Stage | Call | Auth Type | Response |
|---|---|---|---|
| Stage 1 | `getToken` | `none` | `***` ✅ (masked — token used internally for Stage 2) |
| Stage 2 | `fetchCustomer` | `bearerToken` → `Authorization: Bearer <token>` | `{ id: CUST-001, name: Acme Corp, tier: enterprise, creditLimit: 50000 }` ✅ |
| Stage 2 | `checkInventory` | `apiKeyHeader` → `X-API-Key: inv-api-key-mcp-composer-2026` | `{ sku: SKU-7842, name: Industrial Sensor v3, stockLevel: 142, available: true }` ✅ |
| Stage 3 | `createOrder` | `basicAuth` → `Authorization: Basic b3JkZXJzLXN2Yz…` | `{ orderId: ORD-…, status: confirmed }` ✅ |

---

## TC-07 — Happy Path with out-of-stock SKU (`stopOnError: false`, token masked)

| Stage | Call | Auth Type | Response |
|---|---|---|---|
| Stage 1 | `getToken` | `none` | `***` ✅ (masked) |
| Stage 2 | `fetchCustomer` | `bearerToken` | `{ id: CUST-001, name: Acme Corp, tier: enterprise }` ✅ |
| Stage 2 | `checkInventory` | `apiKeyHeader` | `{ sku: SKU-0000, available: false }` ⚠️ continues |
| Stage 3 | `createOrder` | `basicAuth` | `{ status: confirmed }` ✅ |

> `stopOnError: false` — out-of-stock did not abort the pipeline.

---

## TC-11 — Token masking does not break downstream propagation

| Check | Result |
|---|---|
| `getToken` value in MCP response | `***` ✅ |
| `fetchCustomer` call succeeded (token forwarded in `Authorization` header) | `true` ✅ |
| Customer name returned | `Acme Corp` ✅ |
| Order status | `confirmed` ✅ |

---

## All Test Cases

| # | Description | Expected | Actual | Result |
|---|---|---|---|---|
| TC-01 | `tools/list` | Tool `createOrder` with full input schema and `required: [customerId, productSku, quantity]` | Correct schema returned, required array present | ✅ PASS |
| TC-02 | **Happy path** — CUST-001 + SKU-7842, qty 5 | All 3 stages complete, `isError: false`, token masked | All 4 calls succeeded, `getToken: "***"`, `isError: false` | ✅ PASS |
| TC-03 | Unknown tool name | JSON-RPC `-32602` with available tools listed | `"Unknown tool 'deleteEverything'. Available: 'createOrder'"` | ✅ PASS |
| TC-04 | Missing `customerId` argument | JSON-RPC `-32602` **before any network call** (schema validation) | `"missing required argument(s): customerId"` — no outbound request made | ✅ PASS |
| TC-05 | Unknown customer (CUST-999) | `isError: true` with HTTP 404 detail | `[http_error] call 'fetchCustomer' returned HTTP 404`, `isError: true` | ✅ PASS |
| TC-06 | Non-MCP path with `strictMode: true` | HTTP 404 | HTTP 404 | ✅ PASS |
| TC-07 | **Happy path** — out-of-stock SKU, `stopOnError: false` | Inventory returns `available: false`, pipeline continues, token masked | Order confirmed, `checkInventory.available: false`, `getToken: "***"` | ✅ PASS |
| TC-08 | Malformed JSON body | JSON-RPC `-32700 Parse error` | `"Parse error: key must be a string at line 1 column 2"` | ✅ PASS |
| TC-09 | Unsupported method (`resources/list`) | JSON-RPC `-32601 Method not found` | `"Method not supported: resources/list"` | ✅ PASS |
| TC-10 | GET request without SSE Accept header | HTTP 405 | HTTP 405 | ✅ PASS |
| TC-11 | **Token masking** — `maskInOutput: true` output redacted, pipeline propagates internally | `getToken: "***"` in response, `fetchCustomer` still succeeds | Token masked, downstream Bearer call succeeded, order confirmed | ✅ PASS |
| TC-12 | **P4A → Anypoint deploy** — `make build-asset-files` + publish to Exchange | Build pipeline completes, policy published to Anypoint Exchange | Definition + implementation published; version `0.1.0-20260904191722` | ✅ PASS |
| TC-13 | Wrong `Content-Type` (not `application/json`) | JSON-RPC `-32600 Invalid Request` | `"Content-Type must be application/json"` | ✅ PASS |
| TC-14 | `arguments` field not an object | JSON-RPC `-32602 Invalid Params` | `"'arguments' must be a JSON object, got \"bad\""` | ✅ PASS |

**Functional: 13 / 13 PASS | Deploy: 1 / 1 PASS — 14 / 14 total**

---

## TC-12 — P4A → Anypoint Platform Deploy

### What is tested

The P4A build pipeline clones the GitHub repo at the latest commit (`2eb4347`),
runs `make build-asset-files` (which calls `anypoint-cli-v4 pdk policy-project build-asset-files`
to regenerate `src/generated/config.rs` from `definition/gcl.yaml`), compiles the Rust
crate to `wasm32-wasip1`, and publishes the definition + implementation assets to
Anypoint Exchange.

### Pre-deploy fixes in this commit

| Fix | Detail |
|---|---|
| `definition_asset_id` changed to table form | `{ name = "mcp-tool-composer-policy", version = "0.1.0" }` — bare string caused `[object Object]-v1-0` failure |
| `definition/exchange.json` committed | Required at definition root; was only in generated `target/` output |
| `enable_stop_iteration` removed from runtime `pdk` | Flex 1.9.3 does not support the `flex_enable_stop_iteration` ABI command — WASM failed at init. **Re-enabled in #15** by targeting Flex/Omni Gateway ≥ 1.12.0 (the first runtime with the ABI); the terminating handler now buffers headers+body atomically via `into_headers_body_state`. |
| `definition/gcl.yaml` `bindings` map fixed | Invalid YAML map → sequence error in `build-asset-files` (fixed in prior commit) |
| `.project.yaml` committed | Build runner couldn't locate project root (fixed in prior commit) |

### Result — ✅ PASS

**Ran manually via `anypoint-cli-v4 pdk policy-project publish` on 2026-09-04.**

Additional fix found during manual run: `definition_asset_id` in Cargo.toml must be a **plain string** (not a TOML table) — `anypoint-cli-v4` (Node.js) reads the TOML table as `[object Object]`, producing invalid asset IDs. `cargo-anypoint` (Rust) handles both forms. Reverted to string form.

| Step | Command | Outcome |
|---|---|---|
| 1. Regenerate asset files | `make build-asset-files` | All 6 artifacts generated ✅ |
| 2. Compile WASM | `cargo build --target wasm32-wasip1 --release` | `mcp_tool_composer.wasm` (release) ✅ |
| 3. Generate impl GCL | `cargo-anypoint gcl-gen -d mcp-tool-composer-policy -n default ...` | `mcp_tool_composer_implementation.yaml` ✅ |
| 4. Publish definition | `anypoint-cli-v4 pdk policy-project publish` | Published to Exchange ✅ |
| 5. Publish implementation | (same command, continues automatically) | Published to Exchange ✅ |

**Exchange asset IDs published:**
- Definition: `mcp-tool-composer-policy-dev` v`0.1.0-20260904191722`
- Implementation: `mcp-tool-composer-policy-impl-dev` v`0.1.0-20260904191722`
- Exchange URL: `https://anypoint.mulesoft.com/exchange/96a7526c-e657-42de-919e-0b7bdfab7a80/mcp-tool-composer-policy-dev`

---

## Auth Coverage

| API | Auth Type | How credentials are sent | Verified |
|---|---|---|---|
| Auth Service | `none` | No auth — `client_id` in body | ✓ Token extracted via `outputExtract: access_token` |
| Customer API | `bearerToken` | `Authorization: Bearer ${steps.getToken}` | ✓ Token propagated from Stage 1 (masked in output) |
| Inventory API | `apiKeyHeader` | `X-API-Key: inv-api-key-mcp-composer-2026` | ✓ Static key set correctly |
| Orders API | `basicAuth` | `Authorization: Basic <base64(orders-svc:s3cr3t-mcp-2026)>` | ✓ Base64 encoding correct |

---

## Behaviour Changes Since Previous Report (issue-fix round)

These cases were **not tested before** and confirm new correctness guarantees:

| Behaviour | Before | After |
|---|---|---|
| Schema validation (TC-04) | Missing args caused 404 from backend | `-32602` returned immediately, no network call |
| Pipeline failures (TC-05) | `-32603 Internal Error` JSON-RPC error | `isError: true` in `CallToolResult` (MCP-compliant) |
| Success response | No `isError` field | `isError: false` always present on success |
| Content-Type enforcement (TC-13) | Accepted any Content-Type | `-32600` if not `application/json` |
| Arguments type check (TC-14) | Non-object arguments silently became `{}` | `-32602` with type detail |
| DataWeave binding | `bind_vars("payload", …)` → potential panic | `bind_payload(&str)` — correct PDK API, returns `Result` |

---

## `maskInOutput` Feature

```yaml
- name: getToken
  endpoint: https://auth.example.com
  method: POST
  path: /token
  authType: none
  outputExtract: access_token
  maskInOutput: true   # ← token replaced with "***" in MCP response
```

- Real value flows internally through `step_outputs`; `${steps.getToken}` resolves normally.
- Only the final MCP response is redacted.
- Default: `false`.

---

## Known Limitations

| # | Issue | Impact |
|---|---|---|
| L-1 | `outputTransform` with object-literal DataWeave (`#[{...}]`) not supported in Flex 1.9.x | All stage outputs returned in MCP result; filter post-response or wait for PEL update |
| L-2 | Completed stages are not rolled back on failure | Design mutating pipelines with idempotency keys |
| L-3 | ~~`enable_stop_iteration` removed from runtime `pdk` for Flex 1.9.3 compatibility~~ **Resolved in #15**: re-enabled by targeting Flex/Omni Gateway ≥ 1.12.0 | Runtime floor bumped to 1.12.0 (`minRuntimeVersion`, `[package.metadata.flex] min-version`, playground image); handler buffers atomically via `into_headers_body_state` |
| L-4 | A2D mock APIs: `auth_enabled` must be `false` for isolated policy testing | Test-only workaround; real backends validate normally |
