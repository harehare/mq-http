use axum::http::{HeaderMap, Method, Uri, Version, header};
use mq_lang::{Ident, RuntimeValue};
use std::collections::BTreeMap;

pub fn build_request_value(
    remote_addr: &str,
    method: &Method,
    uri: &Uri,
    version: Version,
    headers: &HeaderMap,
    params: BTreeMap<String, String>,
    body_bytes: &[u8],
) -> RuntimeValue {
    let mut req_dict = BTreeMap::new();

    req_dict.insert(
        Ident::new("method"),
        RuntimeValue::String(method.to_string()),
    );
    req_dict.insert(
        Ident::new("path"),
        RuntimeValue::String(uri.path().to_string()),
    );
    req_dict.insert(Ident::new("uri"), RuntimeValue::String(uri.to_string()));
    req_dict.insert(
        Ident::new("version"),
        RuntimeValue::String(format!("{:?}", version)),
    );
    req_dict.insert(
        Ident::new("scheme"),
        RuntimeValue::String(uri.scheme_str().unwrap_or("http").to_string()),
    );
    req_dict.insert(
        Ident::new("remote_addr"),
        RuntimeValue::String(remote_addr.to_owned()),
    );

    let mut query_dict = BTreeMap::new();
    for (k, v) in params {
        query_dict.insert(Ident::new(&k), RuntimeValue::String(v));
    }
    req_dict.insert(Ident::new("query"), RuntimeValue::Dict(query_dict));

    let mut headers_dict = BTreeMap::new();
    let mut cookies_dict = BTreeMap::new();
    for (k, v) in headers.iter() {
        let val_str = v.to_str().unwrap_or_default().to_string();
        headers_dict.insert(
            Ident::new(k.as_str()),
            RuntimeValue::String(val_str.clone()),
        );
        if k == header::COOKIE {
            for cookie in val_str.split(';') {
                if let Some((name, val)) = cookie.trim().split_once('=') {
                    cookies_dict.insert(Ident::new(name), RuntimeValue::String(val.to_string()));
                }
            }
        }
    }
    req_dict.insert(Ident::new("headers"), RuntimeValue::Dict(headers_dict));
    req_dict.insert(Ident::new("cookies"), RuntimeValue::Dict(cookies_dict));

    let body = String::from_utf8_lossy(body_bytes).to_string();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    req_dict.insert(Ident::new("body"), parse_body(&body, content_type));

    RuntimeValue::Dict(req_dict)
}

fn parse_body(body: &str, content_type: &str) -> RuntimeValue {
    if content_type.contains("application/json") {
        serde_json::from_str::<serde_json::Value>(body)
            .map(RuntimeValue::from)
            .unwrap_or_else(|_| RuntimeValue::String(body.to_string()))
    } else if content_type.contains("application/x-www-form-urlencoded") {
        serde_urlencoded::from_str::<BTreeMap<String, String>>(body)
            .map(|m| {
                let mut dict = BTreeMap::new();
                for (k, v) in m {
                    dict.insert(Ident::new(&k), RuntimeValue::String(v));
                }
                RuntimeValue::Dict(dict)
            })
            .unwrap_or_else(|_| RuntimeValue::String(body.to_string()))
    } else if content_type.contains("application/yaml") || content_type.contains("text/yaml") {
        yaml_rust2::YamlLoader::load_from_str(body)
            .ok()
            .and_then(|docs| docs.into_iter().next())
            .map(RuntimeValue::from)
            .unwrap_or_else(|| RuntimeValue::String(body.to_string()))
    } else if content_type.contains("application/toml") {
        body.parse::<toml::Value>()
            .ok()
            .and_then(|v| serde_json::to_value(v).ok())
            .map(RuntimeValue::from)
            .unwrap_or_else(|| RuntimeValue::String(body.to_string()))
    } else {
        RuntimeValue::String(body.to_string())
    }
}
