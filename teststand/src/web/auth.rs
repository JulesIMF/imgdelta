// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Bearer-token authentication middleware for Axum.

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use subtle::ConstantTimeEq;

pub async fn auth_middleware(
    axum::extract::State(token): axum::extract::State<String>,
    req: Request,
    next: Next,
) -> Response {
    // Allow SSE and index without auth for convenience during dev?
    // No — require auth for everything.
    let provided = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");

    let ok: bool = provided.as_bytes().ct_eq(token.as_bytes()).into();

    if ok {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}
