# MCP Tool Composer Policy

A [Flex Gateway PDK](https://docs.mulesoft.com/gateway/latest/policies-pdk-overview) custom policy that exposes a single **MCP tool** backed by a declarative pipeline of REST API calls — with sequential and parallel stages, DataWeave transforms, and per-call authentication.

## Overview

Instead of writing custom integration code, you configure a pipeline of REST calls in the policy configuration. The policy handles:

- Serving an MCP endpoint (`/mcp` by default) via Streamable HTTP transport
- Accepting `tools/list` and `tools/call` JSON-RPC requests
- Running your pipeline stages (sequential by default, parallel opt-in)
- Applying DataWeave expressions to reshape inputs and outputs
- Returning a well-formed MCP tool result

```
MCP Client → tools/call
    ↓
inputTransform (DataWeave)
    ↓
Stage 1: serial call  →  ${steps.call1.*}
    ↓
Stage 2: parallel calls  →  ${steps.call2.*}, ${steps.call3.*}
    ↓
outputTransform (DataWeave)
    ↓
MCP tool result
```

## Configuration

| Parameter | Type | Required | Description |
|---|---|---|---|
| `toolName` | string | ✓ | MCP tool name exposed to clients (e.g. `createOrder`) |
| `toolDescription` | string | ✓ | Human-readable description in `tools/list` |
| `toolInputSchema` | string | | JSON Schema for the tool's input arguments |
| `stages` | array | ✓ | Ordered list of pipeline stages |
| `inputTransform` | DataWeave | | Transforms incoming MCP args before pipeline; result is `${args.*}` |
| `outputTransform` | DataWeave | | Transforms composite pipeline result before MCP response |
| `perRequestTimeoutMs` | integer | | Default per-call timeout in ms (default: 30000) |
| `mcpEndpoint` | string | | Path for the MCP endpoint (default: `/mcp`) |
| `strictMode` | boolean | | Reject non-MCP requests with 404 (default: true) |

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

### Auth types

| `authType` | Description |
|---|---|
| `none` | No authentication |
| `passthrough` | Forwards the incoming `Authorization` header unchanged |
| `bearerToken` | Static or `${steps.*}`-resolved Bearer token |
| `basicAuth` | Username + password |
| `apiKeyHeader` | Custom header name + API key value |
| `customHeaders` | Full control via the `headers` array |

### Expression syntax

- `${args.fieldName}` — value from the (optionally transformed) MCP tool arguments
- `${steps.<callName>}` — full response body of a previous call
- `${steps.<callName>.field.nested}` — dot-notation path into a previous response
- `outputExtract: "data.id"` — extract a sub-path from a call's response into `${steps.<name>}`

## Limits

- Max 10 stages
- Max 5 calls per parallel stage
- Max 10 calls total across all stages

## Local development

### Prerequisites

- [Rust](https://rustup.rs/) ≥ 1.88
- [Docker](https://www.docker.com/) (for the Flex Gateway playground)
- [anypoint-cli-v4](https://docs.mulesoft.com/anypoint-cli/latest/)

### Build

```bash
cargo build --release
```

### Run locally

Start the Flex Gateway playground with the policy applied:

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
