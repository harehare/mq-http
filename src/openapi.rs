/// OpenAPI 3.0 spec generation from mq scripts.
///
/// Extracts route information from `http::get_route` / `http::post_route` etc. calls
/// and combines them with doc-comment annotations written above `def` functions.
///
/// Annotation format (lines starting with `# @` immediately before a `def`):
///
/// ```mq
/// # @summary Short description
/// # @description Longer explanation (multiple lines allowed)
/// # @tag tagName
/// # @param name in type required description
/// # @response 200 application/json "Success"
/// # @response 404 "Not found"
/// def handle_something(r):
///   ...
/// end
/// ```
///
/// `in` for `@param` can be: `query`, `path`, `header`, `cookie`
/// `type` for `@param` can be: `string`, `integer`, `number`, `boolean`, `array`, `object`
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct FuncAnnotation {
    pub summary: Option<String>,
    pub description: Vec<String>,
    pub tags: Vec<String>,
    pub params: Vec<ParamInfo>,
    pub responses: Vec<ResponseInfo>,
}

#[derive(Debug, Clone)]
pub struct ParamInfo {
    pub name: String,
    pub location: String,
    pub schema_type: String,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ResponseInfo {
    pub status: String,
    pub description: String,
    pub content_type: Option<String>,
}

pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub annotation: FuncAnnotation,
}

/// Parse a mq script and return route entries enriched with doc-comment annotations.
pub fn parse_script(content: &str) -> Vec<RouteEntry> {
    let annotations = parse_func_annotations(content);
    let routes = extract_routes(content);

    routes
        .into_iter()
        .map(|(method, path, handler)| {
            let annotation = handler
                .as_deref()
                .and_then(|h| annotations.get(h))
                .cloned()
                .unwrap_or_default();
            RouteEntry {
                method,
                path,
                annotation,
            }
        })
        .collect()
}

/// Scan the script line-by-line and collect `# @tag value` blocks that
/// appear immediately before `def func_name(...)` declarations.
pub(crate) fn parse_func_annotations(content: &str) -> HashMap<String, FuncAnnotation> {
    let mut result = HashMap::new();
    let mut pending: Vec<String> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();

        if let Some(annotation) = trimmed.strip_prefix("# @") {
            pending.push(annotation.to_string());
        } else if trimmed.starts_with("# ") || trimmed == "#" {
            // plain comment — keep accumulating if we already have @-annotations
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            let func_name = rest
                .split(['(', ' '])
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            if !func_name.is_empty() && !pending.is_empty() {
                result.insert(func_name, build_annotation(&pending));
            }
            pending.clear();
        } else if trimmed.is_empty() {
            // blank lines reset the accumulator so annotations must be contiguous
            pending.clear();
        } else {
            pending.clear();
        }
    }

    result
}

fn build_annotation(lines: &[String]) -> FuncAnnotation {
    let mut ann = FuncAnnotation::default();

    for line in lines {
        let (tag, rest) = if let Some(idx) = line.find(' ') {
            (&line[..idx], line[idx + 1..].trim())
        } else {
            (line.as_str(), "")
        };

        match tag {
            "summary" => ann.summary = Some(rest.to_string()),
            "description" => ann.description.push(rest.to_string()),
            "tag" => ann.tags.push(rest.to_string()),
            "param" => {
                // @param <name> <in> <type> <required> <description>
                let parts: Vec<&str> = rest.splitn(5, ' ').collect();
                ann.params.push(ParamInfo {
                    name: parts.first().copied().unwrap_or("").to_string(),
                    location: parts.get(1).copied().unwrap_or("query").to_string(),
                    schema_type: parts.get(2).copied().unwrap_or("string").to_string(),
                    required: parts.get(3).copied().unwrap_or("false") == "true",
                    description: parts
                        .get(4)
                        .copied()
                        .unwrap_or("")
                        .trim_matches('"')
                        .to_string(),
                });
            }
            "response" => {
                // Format: @response <status> [content-type] ["description"]
                // The description may contain spaces, so we split on the first
                // space only to isolate the status, then inspect the remainder.
                let mut parts2 = rest.splitn(2, ' ');
                let status = parts2.next().unwrap_or("200").to_string();
                let remainder = parts2.next().unwrap_or("").trim();

                let (content_type, description) = if remainder.is_empty() {
                    (None, String::new())
                } else if remainder.starts_with('"') {
                    // @response 404 "Not found"
                    (None, remainder.trim_matches('"').to_string())
                } else if let Some(space_pos) = remainder.find(' ') {
                    // @response 200 application/json "Description"
                    let ct = &remainder[..space_pos];
                    let desc = remainder[space_pos + 1..].trim().trim_matches('"');
                    (Some(ct.to_string()), desc.to_string())
                } else if remainder.contains('/') {
                    // @response 200 application/json
                    (Some(remainder.to_string()), String::new())
                } else {
                    (None, remainder.to_string())
                };
                ann.responses.push(ResponseInfo {
                    status,
                    description,
                    content_type,
                });
            }
            _ => {}
        }
    }

    ann
}

/// Extract `(METHOD, path, Option<handler_name>)` tuples from a mq script.
///
/// Handles:
/// - `http::get_route(r, "/path", fn(r): handler(r);)`
/// - `http::route(r, "GET", "/path", fn(r): handler(r);)`
pub(crate) fn extract_routes(content: &str) -> Vec<(String, String, Option<String>)> {
    let preprocessed = inject_pipe_after_top_level_end(content);
    let (nodes, _) = mq_lang::parse_recovery(&preprocessed);
    let mut routes = Vec::new();
    for node in &nodes {
        collect_routes(node, &mut routes);
    }
    routes
}

/// Insert `|` after each top-level `end` so that `def...end` blocks followed
/// by a route expression are connected in the CST without requiring the caller
/// to write explicit pipe separators.
fn inject_pipe_after_top_level_end(content: &str) -> String {
    let mut depth: usize = 0;
    let mut result = String::with_capacity(content.len() + 16);
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("def ") {
            depth += 1;
        }
        result.push_str(line);
        result.push('\n');
        if trimmed == "end" && depth > 0 {
            depth -= 1;
            if depth == 0 {
                result.push_str("|\n");
            }
        }
    }
    result
}

fn collect_routes(node: &mq_lang::CstNode, routes: &mut Vec<(String, String, Option<String>)>) {
    if matches!(node.kind, mq_lang::CstNodeKind::QualifiedAccess)
        && node.token.as_ref().map(|t| t.to_string()).as_deref() == Some("http")
    {
        let func_name = node
            .children
            .iter()
            .find(|c| matches!(c.kind, mq_lang::CstNodeKind::Ident))
            .and_then(|c| c.get_identifier());

        if let Some(func) = func_name {
            let strings: Vec<String> = node
                .children
                .iter()
                .filter_map(|c| extract_string_literal(c.as_ref()))
                .collect();

            let handler = node
                .children
                .iter()
                .find(|c| c.is_fn())
                .and_then(|c| find_first_call_name(c.as_ref()));

            match func.as_str() {
                "get_route" | "post_route" | "put_route" | "patch_route" | "delete_route" => {
                    let method = func.trim_end_matches("_route").to_uppercase();
                    if let Some(path) = strings.first() {
                        routes.push((method, path.clone(), handler));
                    }
                }
                "route" => {
                    if strings.len() >= 2 {
                        routes.push((strings[0].clone(), strings[1].clone(), handler));
                    }
                }
                _ => {}
            }
        }
    }
    for child in &node.children {
        collect_routes(child, routes);
    }
}

fn extract_string_literal(node: &mq_lang::CstNode) -> Option<String> {
    if matches!(node.kind, mq_lang::CstNodeKind::Literal) {
        node.token.as_ref().and_then(|t| {
            if let mq_lang::TokenKind::StringLiteral(s) = &t.kind {
                Some(s.to_string())
            } else {
                None
            }
        })
    } else {
        None
    }
}

fn find_first_call_name(node: &mq_lang::CstNode) -> Option<String> {
    for child in &node.children {
        if matches!(child.kind, mq_lang::CstNodeKind::Call) {
            return child.token.as_ref().map(|t| t.to_string());
        }
        if let Some(name) = find_first_call_name(child) {
            return Some(name);
        }
    }
    None
}

/// Build an OpenAPI 3.0 JSON value from the parsed route entries.
pub fn build_openapi_json(title: &str, version: &str, routes: &[RouteEntry]) -> Value {
    let mut paths: serde_json::Map<String, Value> = serde_json::Map::new();

    for route in routes {
        let path_item = paths.entry(route.path.clone()).or_insert_with(|| json!({}));
        let method = route.method.to_lowercase();
        let ann = &route.annotation;

        // Parameters
        let parameters: Vec<Value> = ann
            .params
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "in": p.location,
                    "required": p.required,
                    "description": p.description,
                    "schema": { "type": p.schema_type },
                })
            })
            .collect();

        // Responses
        let mut responses_map = serde_json::Map::new();
        if ann.responses.is_empty() {
            responses_map.insert("200".to_string(), json!({ "description": "Success" }));
        } else {
            for resp in &ann.responses {
                let mut resp_obj = serde_json::Map::new();
                resp_obj.insert(
                    "description".to_string(),
                    Value::String(resp.description.clone()),
                );
                if let Some(ct) = &resp.content_type {
                    resp_obj.insert(
                        "content".to_string(),
                        json!({ ct: { "schema": { "type": "object" } } }),
                    );
                }
                responses_map.insert(resp.status.clone(), Value::Object(resp_obj));
            }
        }

        // Operation object
        let mut operation = serde_json::Map::new();
        if let Some(s) = &ann.summary {
            operation.insert("summary".to_string(), Value::String(s.clone()));
        }
        if !ann.description.is_empty() {
            operation.insert(
                "description".to_string(),
                Value::String(ann.description.join("\n")),
            );
        }
        if !ann.tags.is_empty() {
            operation.insert("tags".to_string(), json!(ann.tags));
        }
        if !parameters.is_empty() {
            operation.insert("parameters".to_string(), Value::Array(parameters));
        }
        operation.insert("responses".to_string(), Value::Object(responses_map));

        if let Some(path_obj) = path_item.as_object_mut() {
            path_obj.insert(method, Value::Object(operation));
        }
    }

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": title,
            "version": version,
        },
        "paths": paths,
    })
}

/// Inline Swagger UI HTML that loads from CDN and points to `/_openapi.json`.
pub fn swagger_ui_html(title: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>{title} — API Docs</title>
  <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css" />
  <style>
    body {{ margin: 0; }}
    .topbar {{ display: none; }}
  </style>
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
  <script>
    window.onload = function () {{
      SwaggerUIBundle({{
        url: "/_openapi.json",
        dom_id: "#swagger-ui",
        deepLinking: true,
        presets: [SwaggerUIBundle.presets.apis, SwaggerUIBundle.SwaggerUIStandalonePreset],
        layout: "BaseLayout",
      }});
    }};
  </script>
</body>
</html>"##,
        title = title,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    // ---- parse_func_annotations --------------------------------------------

    #[rstest]
    #[case(
        "# @summary Health check\ndef handle_health(_):\n  http::ok(\"\")\nend",
        "handle_health",
        Some("Health check")
    )]
    #[case(
        "# @summary   Leading spaces trimmed  \ndef my_fn(_):\nend",
        "my_fn",
        Some("Leading spaces trimmed")
    )]
    #[case("def no_annotation(_):\nend", "no_annotation", None)]
    fn test_annotation_summary(
        #[case] script: &str,
        #[case] func: &str,
        #[case] expected: Option<&str>,
    ) {
        let annotations = parse_func_annotations(script);
        assert_eq!(
            annotations.get(func).and_then(|a| a.summary.as_deref()),
            expected
        );
    }

    #[rstest]
    #[case(
        "# @tag health\n# @tag api\ndef fn1(_):\nend",
        "fn1",
        &["health", "api"],
    )]
    #[case(
        "# @tag only_one\ndef fn2(_):\nend",
        "fn2",
        &["only_one"],
    )]
    #[case(
        "def fn3(_):\nend",
        "fn3",
        &[],
    )]
    fn test_annotation_tags(
        #[case] script: &str,
        #[case] func: &str,
        #[case] expected_tags: &[&str],
    ) {
        let annotations = parse_func_annotations(script);
        let tags: Vec<&str> = annotations
            .get(func)
            .map(|a| a.tags.iter().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(tags, expected_tags);
    }

    #[test]
    fn test_annotation_param_full() {
        let script = "# @param name query string true \"User name\"\ndef handle(_):\nend";
        let annotations = parse_func_annotations(script);
        let param = &annotations["handle"].params[0];
        assert_eq!(param.name, "name");
        assert_eq!(param.location, "query");
        assert_eq!(param.schema_type, "string");
        assert!(param.required);
        assert_eq!(param.description, "User name");
    }

    #[rstest]
    #[case(
        "# @response 200 application/json \"OK\"\ndef h(_):\nend",
        "h",
        "200",
        Some("application/json"),
        "OK"
    )]
    #[case(
        "# @response 404 \"Not found\"\ndef h2(_):\nend",
        "h2",
        "404",
        None,
        "Not found"
    )]
    #[case("# @response 204\ndef h3(_):\nend", "h3", "204", None, "")]
    fn test_annotation_response(
        #[case] script: &str,
        #[case] func: &str,
        #[case] expected_status: &str,
        #[case] expected_ct: Option<&str>,
        #[case] expected_desc: &str,
    ) {
        let annotations = parse_func_annotations(script);
        let resp = &annotations[func].responses[0];
        assert_eq!(resp.status, expected_status);
        assert_eq!(resp.content_type.as_deref(), expected_ct);
        assert_eq!(resp.description, expected_desc);
    }

    #[test]
    fn test_annotation_reset_on_blank_line() {
        // Blank line between annotation block and def → annotations NOT associated
        let script = "# @summary Orphaned\n\ndef fn_after_blank(_):\nend";
        let annotations = parse_func_annotations(script);
        assert!(
            annotations
                .get("fn_after_blank")
                .and_then(|a| a.summary.as_ref())
                .is_none()
        );
    }

    #[test]
    fn test_annotation_multiple_functions() {
        let script = "# @summary First\ndef fn_a(_):\nend\n# @summary Second\ndef fn_b(_):\nend";
        let annotations = parse_func_annotations(script);
        assert_eq!(annotations["fn_a"].summary.as_deref(), Some("First"));
        assert_eq!(annotations["fn_b"].summary.as_deref(), Some("Second"));
    }

    // ---- extract_routes ----------------------------------------------------

    #[rstest]
    #[case("http::get_route(r, \"/\", fn(r): handle(r);)", "GET", "/")]
    #[case("http::post_route(r, \"/items\", fn(r): h(r);)", "POST", "/items")]
    #[case("http::put_route(r, \"/x\", fn(r): h(r);)", "PUT", "/x")]
    #[case("http::patch_route(r, \"/y\", fn(r): h(r);)", "PATCH", "/y")]
    #[case("http::delete_route(r, \"/z\", fn(r): h(r);)", "DELETE", "/z")]
    fn test_extract_method_routes(
        #[case] script: &str,
        #[case] expected_method: &str,
        #[case] expected_path: &str,
    ) {
        let routes = extract_routes(script);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0, expected_method);
        assert_eq!(routes[0].1, expected_path);
    }

    #[test]
    fn test_extract_route_handler_name() {
        let script = "http::get_route(r, \"/health\", fn(r): handle_health(r);)";
        let routes = extract_routes(script);
        assert_eq!(routes[0].2.as_deref(), Some("handle_health"));
    }

    #[test]
    fn test_extract_generic_route() {
        let script = r#"http::route(r, "DELETE", "/items", fn(r): del(r);)"#;
        let routes = extract_routes(script);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].0, "DELETE");
        assert_eq!(routes[0].1, "/items");
    }

    #[test]
    fn test_extract_multiple_routes() {
        let script = concat!(
            "http::dispatch(req, [\n",
            "  fn(r): http::get_route(r, \"/a\", fn(r): h_a(r););,\n",
            "  fn(r): http::post_route(r, \"/b\", fn(r): h_b(r););,\n",
            "  fn(r): http::delete_route(r, \"/c\", fn(r): h_c(r););,\n",
            "])",
        );
        let routes = extract_routes(script);
        assert_eq!(routes.len(), 3);
    }

    #[test]
    fn test_extract_no_routes() {
        assert!(extract_routes("def handle(_):\n  http::ok(\"\")\nend").is_empty());
    }

    // ---- parse_script (integration) ----------------------------------------

    #[test]
    fn test_parse_script_links_annotations_to_routes() {
        let script = concat!(
            "# @summary Health check\n",
            "# @tag health\n",
            "def handle_health(_):\n",
            "  http::json_ok({\"status\": \"ok\"})\n",
            "end\n",
            "\n",
            "http::dispatch(req, [\n",
            "  fn(r): http::get_route(r, \"/health\", fn(r): handle_health(r););,\n",
            "])",
        );
        let routes = parse_script(script);
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/health");
        assert_eq!(
            routes[0].annotation.summary.as_deref(),
            Some("Health check")
        );
        assert_eq!(routes[0].annotation.tags, vec!["health"]);
    }

    #[test]
    fn test_parse_script_route_without_annotation() {
        let script = "http::get_route(r, \"/bare\", fn(r): h(r);)";
        let routes = parse_script(script);
        assert_eq!(routes.len(), 1);
        assert!(routes[0].annotation.summary.is_none());
        assert!(routes[0].annotation.tags.is_empty());
    }

    // ---- build_openapi_json ------------------------------------------------

    #[test]
    fn test_openapi_json_structure() {
        let routes = parse_script("");
        let spec = build_openapi_json("My API", "2.0.0", &routes);
        assert_eq!(spec["openapi"], "3.0.3");
        assert_eq!(spec["info"]["title"], "My API");
        assert_eq!(spec["info"]["version"], "2.0.0");
    }

    #[test]
    fn test_openapi_json_default_response() {
        let script = "http::get_route(r, \"/ping\", fn(r): h(r);)";
        let routes = parse_script(script);
        let spec = build_openapi_json("T", "1", &routes);
        assert_eq!(
            spec["paths"]["/ping"]["get"]["responses"]["200"]["description"],
            "Success"
        );
    }

    #[test]
    fn test_openapi_json_with_full_annotations() {
        let script = concat!(
            "# @summary List items\n",
            "# @tag items\n",
            "# @param page query integer false \"Page number\"\n",
            "# @response 200 application/json \"Item list\"\n",
            "# @response 400 \"Bad request\"\n",
            "def handle_list(_):\nend\n",
            "http::get_route(r, \"/items\", fn(r): handle_list(r);)",
        );
        let routes = parse_script(script);
        let spec = build_openapi_json("T", "1", &routes);
        let op = &spec["paths"]["/items"]["get"];

        assert_eq!(op["summary"], "List items");
        assert_eq!(op["tags"][0], "items");
        assert_eq!(op["parameters"][0]["name"], "page");
        assert_eq!(op["parameters"][0]["in"], "query");
        assert_eq!(op["parameters"][0]["schema"]["type"], "integer");
        assert_eq!(op["responses"]["200"]["description"], "Item list");
        assert_eq!(op["responses"]["400"]["description"], "Bad request");
    }

    #[rstest]
    #[case("GET", "get")]
    #[case("POST", "post")]
    #[case("PUT", "put")]
    #[case("PATCH", "patch")]
    #[case("DELETE", "delete")]
    fn test_openapi_json_method_lowercase(
        #[case] script_method_keyword: &str,
        #[case] expected_json_key: &str,
    ) {
        let method_lower = script_method_keyword.to_lowercase();
        let script = format!("http::{method_lower}_route(r, \"/x\", fn(r): h(r);)");
        let routes = parse_script(&script);
        let spec = build_openapi_json("T", "1", &routes);
        assert!(spec["paths"]["/x"][expected_json_key].is_object());
    }
}
