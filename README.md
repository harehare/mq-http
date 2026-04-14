<h1 align="center">mq-http</h1>

A lightweight HTTP server that executes [mq](https://mqlang.org/) scripts for each request.

## Installation

```bash
cargo install mq-http
```

## Usage

```bash
# Run with a script file
mq-http script.mq

# Run with an inline script
mq-http -c '"# Hello World"'

# Run on a custom address and port
mq-http -a 0.0.0.0 -p 8080 script.mq

# Hot-reload on file change (requires --features watch)
mq-http --reload script.mq
```

## Request Object

Every request exposes a `req` variable with the following fields:

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

Dict fields are accessed with `get(dict, "key")`.

## Script Examples

### Simple string response

```mq
"# Hello from mq-http"
```

### Path-based routing

```mq
let path = get(req, "path")
| if (path == "/"):
    "# Welcome"
  elif (path == "/health"):
    {"status": 200, "body": "OK"}
  else:
    {"status": 404, "body": "Not Found"}
  end
```

### Reading query parameters

```mq
let name = get(get(req, "query"), "name")
| "Hello, " + name + "!"
```

### Reading a JSON request body

```mq
let body = get(req, "body")
| let name = get(body, "name")
| "Hello, " + name
```

### Returning JSON

```mq
{"name": "mq-http", "version": "0.1.0"}
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
def(r):
  let path = get(r, "path")
  | "You requested: " + path
end
```

## Output Format (`-F`)

Controls how the script's return value is serialised into the HTTP response.

| Flag | `string` response | `Markdown` response |
|------|-------------------|---------------------|
| `markdown` *(default)* | `text/markdown` — raw Markdown | `text/markdown` — raw Markdown |
| `html` | `text/html` — string as-is | `text/html` — rendered to HTML |
| `text` | `text/plain` | `text/plain` — raw Markdown text |
| `json` | `application/json` | — |

`dict` and `array` values are always returned as `application/json` regardless of `-F`.

```bash
# Return raw Markdown (default)
mq-http script.mq

# Render Markdown to HTML
mq-http -F html script.mq
```

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
  -c <COMMAND>           Execute mq script from string
  -p, --port <PORT>      Port to listen on [default: 3000]
  -a, --addr <ADDR>      Bind address [default: 127.0.0.1]
  -F, --format <FORMAT>  Default output format (markdown, html, text, json) [default: markdown]
  -L, --directory <DIR>  Search modules from the directory
      --args <NAME> <VALUE>     Set a named string value
      --rawfile <NAME> <FILE>   Set a named value from file contents
  -r, --reload           Hot-reload script on file change (requires --features watch)
  -h, --help             Print help
```

## Features

| Feature | Description |
|---------|-------------|
| `watch` | Enable `--reload` hot-reload via file watching (`notify` crate) |

```bash
cargo build --features watch
```

## Environment Variables

| Variable | Description |
|----------|-------------|
| `MQ_HTTP_PORT` | Override `--port` |
| `MQ_HTTP_ADDR` | Override `--addr` |


## License

This project is licensed under the MIT License. See the [LICENSE](LICENSE) file for details.

