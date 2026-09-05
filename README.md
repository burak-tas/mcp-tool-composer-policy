# MCP Tool Composer Policy

A [Omni Gateway PDK](https://docs.mulesoft.com/gateway/latest/policies-pdk-overview) custom policy that exposes a single **MCP tool** backed by a **declarative multi-step pipeline** of REST API calls — with sequential and parallel stages, DataWeave transforms, and per-call authentication.

## When to use this policy

| Scenario | Policy to use |
|---|---|
| One MCP tool → one REST API call | **MCP Transcoding** (built-in) |
| **One MCP tool → multiple REST APIs in sequence or parallel** | **This policy** |
| Multiple MCP tools, each with their own backend | Apply this policy once per tool, or use a router pattern |

The key differentiator is **multi-step composition**: a single `tools/call` request drives a chain of REST API calls across multiple services, where later stages can reference outputs from earlier ones via `${steps.<name>}` expressions.

## Overview

```
MCP Client → tools/call
    ↓
inputTransform (DataWeave)
    ↓
Stage 1 sequential:  GET /auth  →  ${steps.getToken}
    ↓
Stage 2 parallel:    GET /customers/${args.id}  +  GET /inventory/${args.sku}
    ↓
Stage 3 sequential:  POST /orders  (uses ${steps.getToken} + ${steps.customerProfile})
    ↓
outputTransform (DataWeave)
    ↓
MCP tool result
```

One policy instance = one MCP tool. To expose multiple composed tools, apply the policy multiple times at different `mcpEndpoint` paths (e.g. `/mcp/createOrder`, `/mcp/refundOrder`).

## Configuration

| Parameter | Type | Required | Default | Description |
|---|---|---|---|---|
| `toolName` | string | ✓ | — | MCP tool name exposed to clients (e.g. `createOrder`) |
| `toolDescription` | string | ✓ | — | Human-readable description in `tools/list` |
| `toolInputSchema` | string | | `{}` | JSON Schema for the tool's input arguments; required fields are validated before pipeline execution |
| `stages` | array | ✓ | — | Ordered list of pipeline stages |
| `inputTransform` | DataWeave | | `#[payload]` | Transforms incoming MCP args before pipeline; result is `${args.*}` |
| `outputTransform` | DataWeave | | `#[payload]` | Transforms composite pipeline result before MCP response |
| `perRequestTimeoutMs` | integer | | 30000 | Default per-call timeout in ms (100–600000) |
| `pipelineTimeoutMs` | integer | | 60000 | Global wall-clock deadline for the entire pipeline in ms (1000–600000) |
| `mcpEndpoint` | string | | `/mcp` | Path for the MCP endpoint |
| `strictMode` | boolean | | `true` | Reject non-MCP requests with 404 |

### Stage configuration

Each stage contains one or more `calls`. Stages run sequentially; within a stage, calls can run in parallel (`parallel: true`).

```yaml
stages:
  - calls:
      - name: fetchUser
        endpoint: https://api.example.com
        method: GET
        path: /users/${args.userId}
        authType: bearerToken
        token: "my-static-token"
  - parallel: true
    calls:
      - name: fetchOrders
        endpoint: https://orders.example.com
        method: POST
        path: /orders/search
        bodyTemplate: '{"customerId": "${steps.fetchUser.id}"}'
        authType: passthrough
      - name: fetchProfile
        endpoint: https://profile.example.com
        method: GET
        path: /profiles/${steps.fetchUser.id}
```

### Per-call options

| Option | Type | Default | Description |
|---|---|---|---|
| `name` | string | required | Unique call name; used in `${steps.<name>}` |
| `endpoint` | string | required | Base URL of the backend service |
| `method` | string | `POST` | HTTP method (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`) |
| `path` | string | `/` | URL path; supports `${args.*}` and `${steps.*}` expressions |
| `bodyTemplate` | string | — | JSON body template; supports `${...}` expressions |
| `authType` | string | `none` | Authentication strategy (see below) |
| `timeoutMs` | integer | — | Per-call timeout override in ms |
| `stopOnError` | boolean | `true` | Abort pipeline on non-2xx; `false` = continue with `null` output |
| `outputExtract` | string | — | Dot-path to extract from response into `${steps.<name>}` |
| `maskInOutput` | boolean | `false` | Replace this call's value with `"***"` in the MCP response (value still flows internally) |

### Auth types

| `authType` | Description |
|---|---|
| `none` | No authentication |
| `passthrough` | Forwards the incoming `Authorization` header unchanged |
| `bearerToken` | Adds `Authorization: Bearer <token>` — `token` supports `${steps.*}` |
| `basicAuth` | Adds `Authorization: Basic <base64(user:pass)>` |
| `apiKeyHeader` | Adds a custom header (`headerName: <value>`) — `apiKey` supports `${steps.*}` |
| `customHeaders` | Full control via the `headers` array |

### Credential model

Supported credential sources:
- **Static** — literal values in `token`, `password`, `apiKey` (marked `security:sensitive` in the schema; encrypted at rest by the platform).
- **Step-derived** — `${steps.<callName>}` resolves to a value obtained at runtime (e.g. a token from an earlier auth call). Use `maskInOutput: true` on the auth call to hide the token from the MCP result while still forwarding it internally.
- **Passthrough** — forwards the incoming MCP request's `Authorization` header unchanged.

**`${env.*}` expressions are not supported.** Referencing one is a hard error — the call fails rather than silently sending a request with a hole in it.

### Credential confidentiality

Credentials are marked `security:sensitive` in the policy schema (`token`, `password`,
`apiKey`, `bodyTemplate`, and each custom header `value`), and the policy treats them as
confidential end-to-end:

- **Sent upstream, never returned.** A credential is attached to the outbound call's auth
  header (`Bearer`, `Basic`, the API-key header, or a custom header) but is never copied
  into the MCP response. If a step *fetches* a token (`${steps.<authCall>}`), set
  `maskInOutput: true` on that auth call so its value renders as `"***"` in the result
  while still resolving internally for downstream calls.
- **Errors are generic.** Transform (`inputTransform`/`outputTransform`) and pipeline
  failures return a fixed message; the underlying DataWeave/transport detail — which can
  echo the payload or a resolved credential — is logged server-side only, never sent to
  the client.

**Passthrough trust boundary.** `authType: passthrough` forwards the incoming MCP request's
`Authorization` header **verbatim** to the configured upstream. Only use it on calls whose
`endpoint` you trust with the caller's credential — the gateway is handing that bearer/token
to the backend as-is. Combine with a JWT/OAuth policy *in front* of this one (see
[Policy ordering](#policy-ordering)) so the forwarded credential is one you have already
validated, and never point a passthrough call at an untrusted or third-party host.

### Expression syntax

- `${args.fieldName}` — value from the (optionally transformed) MCP tool arguments
- `${steps.<callName>}` — full response body of a previous call
- `${steps.<callName>.field.nested}` — dot-notation path into a previous response
- `outputExtract: "data.id"` — extract a sub-path from a call's response into `${steps.<name>}`

### Injection-safe construction

Every caller-controlled value is encoded for the exact position it lands in, so a
malicious MCP argument can only ever be **data**, never request structure:

- **URL (`path`, incl. query string)** — substituted values are percent-encoded
  (RFC 3986). `path: /v1/search?name=${args.city}&count=1` with `city="Berlin&count=100"`
  sends `name=Berlin%26count%3D100` — it cannot inject a second query parameter, and
  a value like `../../admin` cannot traverse the path.
- **`bodyTemplate` (JSON)** — interpolation is JSON-position aware. A value inside a
  string (`"${args.customerId}"`) is emitted as escaped string content; a value in a
  bare position (`${args.quantity}`) is emitted as a complete JSON token. A `"` or `\`
  in the value cannot break out and inject sibling fields, so the hybrid convention
  (quote strings, leave numbers/objects bare) stays correct **and** safe.
- **Headers / credentials** — CR/LF are stripped from substituted values to prevent
  header injection.

**Strict resolution.** An expression that cannot be resolved — a missing `args`/`steps`
key, a malformed `${…}`, or an unsupported prefix such as `${env.*}` — fails the call
with an `isError:true` result naming the offending expression (never the resolved value).
It is never silently substituted with an empty string.

## Policy ordering

Apply this policy as the outermost processing layer. Recommended chain:

```
[Rate Limiting] → [JWT / OAuth] → [MCP Schema Validation] → MCP Tool Composer → backend
```

- **Rate Limiting** — apply before MCP processing to cap request volume.
- **JWT / OAuth token enforcement** — apply before this policy if you want to authenticate the MCP client (use `authType: passthrough` on calls that should inherit that credential).
- **MCP Schema Validation** (built-in) — can validate the MCP envelope before it reaches this policy.
- **MCP Support** (built-in) — handles protocol-level concerns; apply at the gateway level alongside this policy.

## Limits

- Max 10 stages
- Max 5 calls per parallel stage
- Max 10 calls total across all stages
- Global pipeline deadline: configurable via `pipelineTimeoutMs` (default 60 s, max 600 s)

## Known limitations

| # | Limitation |
|---|---|
| L-1 | Output is text-only (`content[0].type: "text"`); `structuredContent` / `outputSchema` are not supported |
| L-2 | Completed stages are **not rolled back** on failure — design mutating pipelines with idempotency keys or compensating actions |
| L-3 | `outputTransform` with object-literal DataWeave syntax (`#[{...}]`) is not supported in Flex 1.9.x (PEL limitation) |
| L-4 | One policy instance = one MCP tool; apply multiple times for multiple tools |
| L-5 | Protocol version is fixed at `2024-11-05` (HTTP+SSE transport) |

## Local development

### Prerequisites

- [Rust](https://rustup.rs/) ≥ 1.88
- [Docker](https://www.docker.com/) (for the Omni Gateway playground)
- [anypoint-cli-v4](https://docs.mulesoft.com/anypoint-cli/latest/)

### Build

```bash
~/.cargo/bin/cargo build --target wasm32-wasip1 --release
```

### Run locally

Start the Omni Gateway playground with the policy applied:

```bash
make run
```

Test with curl:

```bash
# List available tools
curl -X POST http://localhost:8081/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'

# Call the tool
curl -X POST http://localhost:8081/mcp \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"<toolName>","arguments":{}}}'
```

### Unit tests

```bash
cargo test
```

## Publishing to Exchange

```bash
make publish
```

Requires `ANYPOINT_TOKEN` and `ANYPOINT_ORG` environment variables (or interactive login via `anypoint-cli-v4`).

## License

Copyright 2026 Salesforce, Inc. All rights reserved.
