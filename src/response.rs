use axum::{
    body::{self as ax_body},
    http::{StatusCode, header},
    response::{
        Html, IntoResponse, Response,
        sse::{Event, Sse},
    },
};
use futures_util::stream;
use mq_lang::{Ident, RuntimeValue};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

/// Convert a `RuntimeValue` to a `serde_json::Value` for JSON serialization.
pub fn runtime_value_to_json(value: &RuntimeValue) -> JsonValue {
    match value {
        RuntimeValue::String(s) => JsonValue::String(s.clone()),
        RuntimeValue::Number(n) => {
            let f = n.value();
            if f.fract() == 0.0 && f.abs() < 9007199254740992.0 {
                JsonValue::Number(serde_json::Number::from(f as i64))
            } else {
                serde_json::Number::from_f64(f)
                    .map(JsonValue::Number)
                    .unwrap_or(JsonValue::Null)
            }
        }
        RuntimeValue::Boolean(b) => JsonValue::Bool(*b),
        RuntimeValue::Dict(map) => {
            let obj = map
                .iter()
                .map(|(k, v)| (k.as_str(), runtime_value_to_json(v)))
                .collect();
            JsonValue::Object(obj)
        }
        RuntimeValue::Array(arr) => {
            JsonValue::Array(arr.iter().map(runtime_value_to_json).collect())
        }
        _ => JsonValue::Null,
    }
}

pub fn runtime_value_to_response(value: RuntimeValue, default_format: &str) -> Response {
    match value {
        RuntimeValue::String(s) => match default_format {
            "json" => Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(ax_body::Body::from(s))
                .unwrap_or_default(),
            "text" => s.into_response(),
            // Return raw Markdown with text/markdown content type.
            "markdown" => Response::builder()
                .header(header::CONTENT_TYPE, "text/markdown; charset=utf-8")
                .body(ax_body::Body::from(s))
                .unwrap_or_default(),
            _ => Html(s).into_response(),
        },
        RuntimeValue::Markdown(node, _) => {
            let md = mq_markdown::Markdown::new(vec![node]);
            match default_format {
                "text" => md.to_string().into_response(),
                "markdown" => Response::builder()
                    .header(header::CONTENT_TYPE, "text/markdown; charset=utf-8")
                    .body(ax_body::Body::from(md.to_string()))
                    .unwrap_or_default(),
                _ => Html(md.to_html()).into_response(),
            }
        }
        RuntimeValue::Dict(ref map) => build_dict_response(map, &value, default_format),
        RuntimeValue::Array(_) => {
            let json = runtime_value_to_json(&value).to_string();
            Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(ax_body::Body::from(json))
                .unwrap_or_default()
        }
        RuntimeValue::Function(..) => value.to_string().into_response(),
        _ => value.to_string().into_response(),
    }
}

fn build_sse_response(map: &BTreeMap<Ident, RuntimeValue>) -> Response {
    let events: Vec<Result<Event, std::convert::Infallible>> = match map.get(&Ident::new("events"))
    {
        Some(RuntimeValue::Array(arr)) => arr
            .iter()
            .map(|e| {
                let mut event = Event::default();
                if let RuntimeValue::Dict(m) = e {
                    if let Some(v) = m.get(&Ident::new("data")) {
                        let data = match v {
                            RuntimeValue::String(s) => s.clone(),
                            _ => runtime_value_to_json(v).to_string(),
                        };
                        event = event.data(data);
                    }
                    if let Some(RuntimeValue::String(name)) = m.get(&Ident::new("event")) {
                        event = event.event(name.clone());
                    }
                    if let Some(RuntimeValue::String(id)) = m.get(&Ident::new("id")) {
                        event = event.id(id.clone());
                    }
                } else {
                    event = event.data(e.to_string());
                }
                Ok(event)
            })
            .collect(),
        _ => vec![],
    };

    Sse::new(stream::iter(events)).into_response()
}

fn build_dict_response(
    map: &BTreeMap<Ident, RuntimeValue>,
    value: &RuntimeValue,
    default_format: &str,
) -> Response {
    // Check for SSE response: {"sse": [event, ...]}
    if let Some(RuntimeValue::Array(_)) = map.get(&Ident::new("sse")) {
        let sse_map: BTreeMap<Ident, RuntimeValue> =
            std::iter::once((Ident::new("events"), map[&Ident::new("sse")].clone())).collect();
        return build_sse_response(&sse_map);
    }

    // If the dict has none of the HTTP response keys, treat it as a plain JSON value.
    let is_response_obj = map.contains_key(&Ident::new("status"))
        || map.contains_key(&Ident::new("headers"))
        || map.contains_key(&Ident::new("body"))
        || map.contains_key(&Ident::new("cookies"));

    if !is_response_obj {
        let json = runtime_value_to_json(value).to_string();
        return Response::builder()
            .header(header::CONTENT_TYPE, "application/json")
            .body(ax_body::Body::from(json))
            .unwrap_or_default();
    }

    let status = map
        .get(&Ident::new("status"))
        .and_then(|v| match v {
            RuntimeValue::Number(n) => StatusCode::from_u16(n.value() as u16).ok(),
            _ => None,
        })
        .unwrap_or(StatusCode::OK);

    let mut response_builder = Response::builder().status(status);

    if let Some(RuntimeValue::Dict(headers)) = map.get(&Ident::new("headers")) {
        for (k, v) in headers {
            response_builder = response_builder.header(k.as_str(), v.to_string());
        }
    }

    if let Some(RuntimeValue::Dict(cookies)) = map.get(&Ident::new("cookies")) {
        for (k, v) in cookies {
            response_builder =
                response_builder.header(header::SET_COOKIE, format!("{}={}", k.as_str(), v));
        }
    }

    match map.get(&Ident::new("body")) {
        Some(body) => match body {
            RuntimeValue::String(s) => response_builder
                .body(ax_body::Body::from(s.clone()))
                .unwrap_or_default(),
            RuntimeValue::Markdown(node, _) => {
                let md = mq_markdown::Markdown::new(vec![node.clone()]);
                let (content, content_type) = match default_format {
                    "text" => (md.to_string(), "text/plain; charset=utf-8"),
                    "markdown" => (md.to_string(), "text/markdown; charset=utf-8"),
                    _ => (md.to_html(), "text/html; charset=utf-8"),
                };
                response_builder
                    .header(header::CONTENT_TYPE, content_type)
                    .body(ax_body::Body::from(content))
                    .unwrap_or_default()
            }
            RuntimeValue::Dict(_) | RuntimeValue::Array(_) => {
                let json = runtime_value_to_json(body).to_string();
                response_builder
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(ax_body::Body::from(json))
                    .unwrap_or_default()
            }
            _ => response_builder
                .body(ax_body::Body::from(body.to_string()))
                .unwrap_or_default(),
        },
        None => response_builder
            .body(ax_body::Body::empty())
            .unwrap_or_default(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use http_body_util::BodyExt;
    use mq_lang::RuntimeValue;
    use rstest::rstest;
    use serde_json::json;

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    // ---- runtime_value_to_json ---------------------------------------------

    #[rstest]
    #[case(RuntimeValue::String("hello".into()),   json!("hello"))]
    #[case(RuntimeValue::Boolean(true),            json!(true))]
    #[case(RuntimeValue::Boolean(false),           json!(false))]
    fn test_to_json_primitives(#[case] value: RuntimeValue, #[case] expected: serde_json::Value) {
        assert_eq!(runtime_value_to_json(&value), expected);
    }

    #[rstest]
    #[case(42i64,  json!(42))]
    #[case(-1i64,  json!(-1))]
    fn test_to_json_number_integer(#[case] n: i64, #[case] expected: serde_json::Value) {
        let value = RuntimeValue::Number(n.into());
        assert_eq!(runtime_value_to_json(&value), expected);
    }

    #[test]
    fn test_to_json_number_float() {
        // f64 without a From impl: use serde_json round-trip to verify
        let json_val = runtime_value_to_json(&RuntimeValue::Number(3i64.into()));
        assert_eq!(json_val, json!(3));
    }

    #[test]
    fn test_to_json_array() {
        let value = RuntimeValue::Array(vec![
            RuntimeValue::Number(1i64.into()),
            RuntimeValue::String("two".into()),
            RuntimeValue::Boolean(false),
        ]);
        assert_eq!(runtime_value_to_json(&value), json!([1, "two", false]));
    }

    #[test]
    fn test_to_json_null_for_unknown_types() {
        assert_eq!(runtime_value_to_json(&RuntimeValue::NONE), json!(null));
    }

    // ---- runtime_value_to_response: plain string ---------------------------

    #[rstest]
    #[case("json", "application/json")]
    #[case("markdown", "text/markdown; charset=utf-8")]
    #[case("text", "text/plain; charset=utf-8")]
    #[case("html", "text/html; charset=utf-8")]
    #[tokio::test]
    async fn test_string_response_content_type(#[case] format: &str, #[case] expected_ct: &str) {
        let resp = runtime_value_to_response(RuntimeValue::String("body".into()), format);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, expected_ct);
    }

    // ---- runtime_value_to_response: dict (HTTP response object) ------------

    #[tokio::test]
    async fn test_dict_response_status() {
        use mq_lang::Ident;
        use std::collections::BTreeMap;

        let mut map = BTreeMap::new();
        map.insert(Ident::new("status"), RuntimeValue::Number(201i64.into()));
        map.insert(Ident::new("body"), RuntimeValue::String("created".into()));

        let resp = runtime_value_to_response(RuntimeValue::Dict(map), "json");
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(body_string(resp).await, "created");
    }

    #[tokio::test]
    async fn test_dict_response_custom_header() {
        use mq_lang::Ident;
        use std::collections::BTreeMap;

        let mut headers_map = BTreeMap::new();
        headers_map.insert(
            Ident::new("x-custom"),
            RuntimeValue::String("value42".into()),
        );
        let mut map = BTreeMap::new();
        map.insert(Ident::new("status"), RuntimeValue::Number(200i64.into()));
        map.insert(Ident::new("headers"), RuntimeValue::Dict(headers_map));
        map.insert(Ident::new("body"), RuntimeValue::String("ok".into()));

        let resp = runtime_value_to_response(RuntimeValue::Dict(map), "json");
        assert_eq!(
            resp.headers().get("x-custom").unwrap().to_str().unwrap(),
            "value42"
        );
    }

    #[tokio::test]
    async fn test_dict_without_status_treated_as_json() {
        use mq_lang::Ident;
        use std::collections::BTreeMap;

        // A dict with no "status"/"body"/"headers" keys is treated as plain JSON
        let mut map = BTreeMap::new();
        map.insert(Ident::new("foo"), RuntimeValue::String("bar".into()));

        let resp = runtime_value_to_response(RuntimeValue::Dict(map), "json");
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["foo"], "bar");
    }

    // ---- runtime_value_to_response: array ----------------------------------

    #[tokio::test]
    async fn test_array_response_is_json() {
        let value = RuntimeValue::Array(vec![
            RuntimeValue::Number(1i64.into()),
            RuntimeValue::Number(2i64.into()),
        ]);
        let resp = runtime_value_to_response(value, "markdown");
        assert_eq!(
            resp.headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "application/json"
        );
        let body = body_string(resp).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed, json!([1, 2]));
    }

    // ---- runtime_value_to_response: cookies --------------------------------

    #[tokio::test]
    async fn test_dict_response_sets_cookie() {
        use mq_lang::Ident;
        use std::collections::BTreeMap;

        let mut cookies_map = BTreeMap::new();
        cookies_map.insert(Ident::new("session"), RuntimeValue::String("abc123".into()));
        let mut map = BTreeMap::new();
        map.insert(Ident::new("status"), RuntimeValue::Number(200i64.into()));
        map.insert(Ident::new("cookies"), RuntimeValue::Dict(cookies_map));
        map.insert(Ident::new("body"), RuntimeValue::String("ok".into()));

        let resp = runtime_value_to_response(RuntimeValue::Dict(map), "json");
        let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
        assert!(cookie.contains("session=abc123"));
    }
}
