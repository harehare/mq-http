use axum::{
    body::{self as ax_body},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response, sse::{Event, Sse}},
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
    let events: Vec<Result<Event, std::convert::Infallible>> =
        match map.get(&Ident::new("events")) {
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
        let sse_map: BTreeMap<Ident, RuntimeValue> = std::iter::once((
            Ident::new("events"),
            map[&Ident::new("sse")].clone(),
        ))
        .collect();
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
            response_builder = response_builder.header(
                header::SET_COOKIE,
                format!("{}={}", k.as_str(), v),
            );
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
