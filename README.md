<div align="center">
  <img src="assets/logo.svg" width="80" height="80" alt="mq-http logo" /><br/>
</div>

<h1 align="center">
  mq-http
</h1>

A lightweight HTTP server that executes [mq](https://mqlang.org/) scripts for each request.

[![ci](https://github.com/harehare/mq-http/actions/workflows/ci.yml/badge.svg)](https://github.com/harehare/mq-http/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/mq-http)](https://crates.io/crates/mq-http)

## Installation

### Using the Installation Script (Recommended)

```bash
curl -fsSL https://raw.githubusercontent.com/harehare/mq-http/main/bin/install.sh | bash
```

The installer will:
- Download the latest release for your platform
- Verify the binary with SHA256 checksum
- Install to `~/.local/bin/`
- Update your shell profile (bash, zsh, or fish)

After installation, restart your terminal or run:

```bash
source ~/.bashrc  # or ~/.zshrc, or ~/.config/fish/config.fish
```

### Cargo

```bash
# Install from crates.io
cargo install mq-http
```

### From Source

```bash
git clone https://github.com/harehare/mq-http.git
cd mq-http
cargo build --release
# Binary will be at target/release/mq-http
```

## Usage

```bash
# Run with a script file
mq-http script.mq

# Run with an inline script
mq-http -c 'http::json_ok({"hello": "world"})'

# Pipe a script from stdin
echo 'http::ok("hello")' | mq-http

# Read from stdin explicitly
mq-http --stdin < script.mq

# Run on a custom address and port
mq-http -a 0.0.0.0 -p 8080 script.mq

# Listen on a Unix domain socket (Unix only)
mq-http --socket /tmp/mq.sock script.mq

# Hot-reload on file change
mq-http --reload script.mq
```

## Request Object

Every script has access to a `req` variable with the following fields:

| Field | Type | Description |
|-------|------|-------------|
| `method` | string | HTTP method (`GET`, `POST`, …) |
| `path` | string | Request path (e.g. `/hello`) |
| `uri` | string | Full URI |
| `scheme` | string | `http` or `https` |
| `version` | string | HTTP version |
| `remote_addr` | string | Client IP and port |
| `query` | dict | Query parameters |
| `headers` | dict | Request headers (lowercase keys) |
| `cookies` | dict | Parsed cookies |
| `body` | string / dict | Body — parsed automatically for JSON, form, YAML, TOML |

## Built-in `http` Module

The `http` module is embedded in the binary and available in every script as `http::*` — no imports needed.

### Response builders

| Function | Status | Description |
|----------|--------|-------------|
| `http::ok(body)` | 200 | Plain response |
| `http::created(body)` | 201 | |
| `http::no_content()` | 204 | |
| `http::bad_request(msg)` | 400 | |
| `http::not_found()` | 404 | |
| `http::method_not_allowed()` | 405 | |
| `http::internal_error(msg)` | 500 | |
| `http::json_ok(body)` | 200 | `application/json` |
| `http::json_created(body)` | 201 | `application/json` |
| `http::json_error(status, msg)` | — | `{"error": msg}` |
| `http::html_ok(body)` | 200 | `text/html` |
| `http::json_response(status, body)` | — | `application/json` with custom status |

### Request helpers

| Function | Description |
|----------|-------------|
| `http::query_param(req, name, default)` | Get a query parameter with fallback |
| `http::req_header(req, name)` | Get a request header by lowercase name |
| `http::is_get(req)` / `http::is_post(req)` / … | Method predicates |

### Router

| Function | Description |
|----------|-------------|
| `http::route(req, method, path, handler)` | Match method + exact path |
| `http::route_prefix(req, method, prefix, handler)` | Match method + path prefix |
| `http::get_route(req, path, handler)` | Shorthand for `GET` |
| `http::post_route(req, path, handler)` | Shorthand for `POST` |
| `http::put_route(req, path, handler)` | Shorthand for `PUT` |
| `http::patch_route(req, path, handler)` | Shorthand for `PATCH` |
| `http::delete_route(req, path, handler)` | Shorthand for `DELETE` |
| `http::dispatch(req, handlers)` | Try handlers in order; falls back to 404 |
| `http::path_eq(req, path)` | Exact path match predicate |
| `http::path_prefix(req, prefix)` | Prefix match predicate |
| `http::path_segments(req)` | Split path into `["a", "b", "c"]` |

### Server-Sent Events

| Function | Description |
|----------|-------------|
| `http::sse(events)` | Return an SSE stream (`text/event-stream`) |
| `http::sse_event(data)` | Build a plain data event |
| `http::sse_event_named(event, data)` | Build a named event with an event type |

## Script Examples

### Simple response

```mq
http::ok("Hello from mq-http!")
```

### JSON response

```mq
http::json_ok({"name": "mq-http", "version": "0.1.0"})
```

### Reading query parameters

```mq
let name = http::query_param(req, "name", "world")
| http::json_ok({"message": "Hello, " + name + "!"})
```

### Path-based routing with `dispatch`

```mq
def handle_root(_):
  http::html_ok("<h1>Welcome</h1>")
end

def handle_health(_):
  http::json_ok({"status": "ok"})
end

def handle_echo(r):
  http::json_ok(r["body"])
end

http::dispatch(req, [
  fn(r): http::get_route(r,  "/",       fn(r): handle_root(r););,
  fn(r): http::get_route(r,  "/health", fn(r): handle_health(r););,
  fn(r): http::post_route(r, "/echo",   fn(r): handle_echo(r););,
])
```

### Full response control

Return a dict with `status`, `headers`, `cookies`, and/or `body`:

```mq
{
  "status": 201,
  "headers": {"x-custom": "value"},
  "body": "Created"
}
```

### Function as handler

If the script evaluates to a function, it is called with `req`:

```mq
fn(r):
  http::json_ok({"path": r["path"], "method": r["method"]})
end
```

### stdin — quick one-liners

```bash
# Pipe a script directly
echo 'http::json_ok({"path": req["path"]})' | mq-http

# Heredoc
mq-http <<'EOF'
http::dispatch(req, [
  fn(r): http::get_route(r, "/", fn(_): http::ok("hi"););,
])
EOF
```

### Server-Sent Events

Return an SSE stream from any route by calling `http::sse(events)`:

```mq
http::sse([
  http::sse_event("hello"),
  http::sse_event("world"),
  http::sse_event_named("done", "finished"),
])
```

Named events work with `EventSource.addEventListener("done", ...)` in the browser.
Each event can also carry JSON data:

```mq
http::sse([
  http::sse_event({"user": "alice", "score": 42}),
  http::sse_event({"user": "bob",   "score": 17}),
])
```

### Unix domain socket

Start the server on a Unix socket instead of a TCP port:

```bash
mq-http --socket /tmp/mq.sock -c 'http::json_ok({"ok": true})'

# Query it with curl
curl --unix-socket /tmp/mq.sock http://localhost/
```

## Output Format (`-F`)

Controls how the script's return value is serialised into the HTTP response.

| Flag | `string` response | `Markdown` response |
|------|-------------------|---------------------|
| `markdown` *(default)* | `text/markdown` | `text/markdown` |
| `html` | `text/html` | `text/html` — rendered to HTML |
| `text` | `text/plain` | `text/plain` |
| `json` | `application/json` | — |

`dict` and `array` values are always returned as `application/json` regardless of `-F`.

## Body Parsing

Request bodies are parsed automatically based on `Content-Type`:

| Content-Type | Parsed as |
|--------------|-----------|
| `application/json` | dict / array |
| `application/x-www-form-urlencoded` | dict |
| `application/yaml` / `text/yaml` | dict |
| `application/toml` | dict |
| anything else | string |

## Options

```
Usage: mq-http [OPTIONS] [SCRIPT]

Arguments:
  [SCRIPT]  Path to the mq script

Options:
  -c, --command <COMMAND>
          Execute mq script from string
  -p, --port <PORT>
          Port to listen on [default: 3000]
  -a, --addr <ADDR>
          Bind address [default: 127.0.0.1]
  -F, --format <FORMAT>
          Default output format (markdown, html, text, json) [default: markdown]
  -L, --directory <MODULE_DIRECTORIES>
          Search modules from the directory
      --args <NAME> <VALUE>
          Sets string that can be referenced at runtime
      --rawfile <NAME> <FILE>
          Sets file contents that can be referenced at runtime
  -r, --reload
          Automatically reload the script when it changes
      --otel-endpoint <OTEL_ENDPOINT>
          OpenTelemetry OTLP endpoint (e.g., http://localhost:4318) [env: OTEL_EXPORTER_OTLP_ENDPOINT=]
      --otel-service-name <OTEL_SERVICE_NAME>
          OpenTelemetry service name [env: OTEL_SERVICE_NAME=] [default: mq-http]
      --tls-cert <TLS_CERT>
          Path to TLS certificate file (PEM)
      --tls-key <TLS_KEY>
          Path to TLS private key file (PEM)
      --stdin
          Read the mq script from stdin
      --docs
          Enable /_docs (Swagger UI) and /_openapi.json endpoints
      --docs-title <DOCS_TITLE>
          API title shown in Swagger UI (requires --docs) [default: API]
      --docs-version <DOCS_VERSION>
          API version shown in Swagger UI (requires --docs) [default: 0.1.0]
      --cors-origins <CORS_ORIGINS>
          Allowed CORS origins (comma-separated). Use '*' to allow all origins
      --timeout <TIMEOUT>
          Request timeout in seconds
      --rate-limit <RATE_LIMIT>
          Rate limit: max requests per second per IP
      --api-key <API_KEY>
          Required API key (via X-Api-Key header or Authorization: Bearer <key>) [env: MQ_HTTP_API_KEY=]
      --basic-auth <BASIC_AUTH>
          Required Basic auth credentials in user:password format [env: MQ_HTTP_BASIC_AUTH=]
      --request-id
          Attach X-Request-Id to every response
      --socket <SOCKET>
          Path to a Unix domain socket to listen on (mutually exclusive with --port/--addr)
  -h, --help
          Print help
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `MQ_HTTP_PORT` | Override `--port` |
| `MQ_HTTP_ADDR` | Override `--addr` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | Override `--otel-endpoint` |
| `OTEL_SERVICE_NAME` | Override `--otel-service-name` |

## Acknowledgements

Inspired by [http-nu](https://github.com/cablehead/http-nu).

## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
