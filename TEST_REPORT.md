# Test Report — MCP Tool Composer Policy

**Date:** 2026-09-02 | **Flex Gateway:** 1.9.3 | **Policy:** v0.1.0
**Backend:** 4 A2D mock REST APIs | **Pipeline:** 3 stages, 4 calls

---

## Pipeline under test

```
Stage 1 (sequential)   Auth Service    → none auth      → extracts access_token
Stage 2 (parallel)     Customer API    → Bearer token   → returns customer profile
                       Inventory API   → X-API-Key      → returns stock level
Stage 3 (sequential)   Orders API      → Basic auth     → creates order
```

---

## TC-02 — Happy Path (all stages successful)

| Stage | Call | Auth Type | Response |
|---|---|---|---|
| Stage 1 | `getToken` | `none` | `access_token: eyJhbGci...` ✅ |
| Stage 2 | `fetchCustomer` | `bearerToken` → `Authorization: Bearer <token>` | `{ id: CUST-001, name: Acme Corp, tier: enterprise, creditLimit: 50000 }` ✅ |
| Stage 2 | `checkInventory` | `apiKeyHeader` → `X-API-Key: inv-api-key-mcp-composer-2026` | `{ sku: SKU-7842, name: Industrial Sensor v3, stockLevel: 142, available: true }` ✅ |
| Stage 3 | `createOrder` | `basicAuth` → `Authorization: Basic b3JkZXJzLXN2Yz…` | `{ orderId: ORD-20260902-0041, status: confirmed, totalAmount: 1499.95 EUR, estimatedDelivery: 2026-09-05 }` ✅ |

---

## TC-07 — Happy Path with out-of-stock SKU (`stopOnError: false`)

| Stage | Call | Auth Type | Response |
|---|---|---|---|
| Stage 1 | `getToken` | `none` | `access_token: eyJhbGci...` ✅ |
| Stage 2 | `fetchCustomer` | `bearerToken` | `{ id: CUST-001, name: Acme Corp, tier: enterprise, creditLimit: 50000 }` ✅ |
| Stage 2 | `checkInventory` | `apiKeyHeader` | `{ sku: SKU-0000, name: Unknown Product, stockLevel: 0, available: false }` ⚠️ continues |
| Stage 3 | `createOrder` | `basicAuth` | `{ orderId: ORD-20260902-0041, status: confirmed, totalAmount: 1499.95 EUR }` ✅ |

> Stage 2 inventory call returned out-of-stock data but did **not** abort the pipeline — `stopOnError: false` worked as designed.

---

## All Test Cases

| # | Description | Expected | Actual | Result |
|---|---|---|---|---|
| TC-01 | `tools/list` | Tool `createOrder` with full input schema | Correct schema returned | ✅ PASS |
| TC-02 | **Happy path** — CUST-001 + SKU-7842, qty 5 | All 3 stages complete, full order response | All 4 calls succeeded — see stage table above | ✅ PASS |
| TC-03 | Unknown tool name | JSON-RPC `-32602` with available tools listed | `"Unknown tool 'deleteEverything'. Available: 'createOrder'"` | ✅ PASS |
| TC-04 | Missing `customerId` argument | Pipeline aborts at `fetchCustomer` | `-32603` · `call: fetchCustomer, httpStatus: 404` | ✅ PASS |
| TC-05 | Unknown customer (CUST-999) | Mock returns 404 → pipeline aborts | `-32603` · `call: fetchCustomer, httpStatus: 404` | ✅ PASS |
| TC-06 | Non-MCP path with `strictMode: true` | HTTP 404 | HTTP 404 | ✅ PASS |
| TC-07 | **Happy path** — out-of-stock SKU, `stopOnError: false` | Inventory returns `available: false`, pipeline continues | Order confirmed, `checkInventory.available: false` — see stage table above | ✅ PASS |
| TC-08 | Malformed JSON body | JSON-RPC `-32700 Parse error` | `"Parse error: key must be a string"` | ✅ PASS |
| TC-09 | Unsupported method (`resources/list`) | JSON-RPC `-32601 Method not found` | `"Method not supported: resources/list"` | ✅ PASS |
| TC-10 | GET request without SSE Accept header | HTTP 405 | HTTP 405 | ✅ PASS |

**Overall: 10 / 10 PASS**

---

## Auth Coverage

| API | Auth Type | How credentials are sent | Verified |
|---|---|---|---|
| Auth Service | `none` | No auth — `client_id` in body | ✓ Token extracted via `outputExtract: access_token` |
| Customer API | `bearerToken` | `Authorization: Bearer ${steps.getToken}` | ✓ Token propagated from Stage 1 |
| Inventory API | `apiKeyHeader` | `X-API-Key: inv-api-key-mcp-composer-2026` | ✓ Static key set correctly |
| Orders API | `basicAuth` | `Authorization: Basic <base64(orders-svc:s3cr3t-mcp-2026)>` | ✓ Base64 encoding correct |

---

## Known Limitations

| # | Issue | Impact |
|---|---|---|
| L-1 | `outputTransform` with object-literal syntax (`#[{...}]`) not supported in Flex 1.9.3 | All stage outputs including raw token are visible in the MCP result — should be filtered in production |
| L-2 | A2D mock APIs enforce credential validation server-side — `auth_enabled` must be `false` for isolated policy testing | Test-only workaround; real backends validate normally |
