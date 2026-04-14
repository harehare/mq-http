use crate::engine::{create_engine, load_raw_files};
use crate::request::build_request_value;
use crate::response::runtime_value_to_response;
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, Method, StatusCode, Uri, Version},
    response::{IntoResponse, Response},
};
use mq_lang::RuntimeValue;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

pub async fn handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>,
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    version: Version,
    headers: HeaderMap,
    Query(params): Query<BTreeMap<String, String>>,
    body_bytes: Bytes,
) -> Response {
    let script = match state.script_content.read().unwrap().clone() {
        Some(s) => s,
        None => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "Script not loaded").into_response();
        }
    };

    let req_value = build_request_value(
        remote_addr,
        &method,
        &uri,
        version,
        &headers,
        params,
        &body_bytes,
    );

    run_script(&state, &script, req_value)
}

fn run_script(state: &AppState, user_script: &str, req_value: RuntimeValue) -> Response {
    // Create a fresh engine per request - fully isolated, no shared mutable state.
    let mut engine = create_engine(&state.args);
    if let Err(e) = load_raw_files(&engine, &state.args) {
        tracing::error!("Failed to load raw files: {:?}", e);
    }

    // `let req = .` makes the request available as a named variable in scripts.
    let full_script = format!("let req = . | {}", user_script);

    match engine.eval(&full_script, std::iter::once(req_value.clone())) {
        Ok(values) => {
            let value = values
                .values()
                .last()
                .cloned()
                .unwrap_or(RuntimeValue::NONE);

            if value.is_function() {
                // Script is a function handler — re-evaluate calling it with req.
                call_function_handler(state, user_script, req_value)
            } else {
                runtime_value_to_response(value, &state.args.format)
            }
        }
        Err(e) => {
            tracing::error!("Script error: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Script error: {:?}", e),
            )
                .into_response()
        }
    }
}

/// When a script evaluates to a function, call it with `req` as the argument.
fn call_function_handler(state: &AppState, user_script: &str, req_value: RuntimeValue) -> Response {
    let mut engine = create_engine(&state.args);
    if let Err(e) = load_raw_files(&engine, &state.args) {
        tracing::error!("Failed to load raw files: {:?}", e);
    }

    // Wrap script in parens and invoke it with req.
    let call_code = format!("let req = . | let _h = ({}) | _h(req)", user_script);

    match engine.eval(&call_code, std::iter::once(req_value)) {
        Ok(values) => {
            let value = values
                .values()
                .last()
                .cloned()
                .unwrap_or(RuntimeValue::NONE);
            runtime_value_to_response(value, &state.args.format)
        }
        Err(e) => {
            tracing::error!("Function call error: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Function call error: {:?}", e),
            )
                .into_response()
        }
    }
}
