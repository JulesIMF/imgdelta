// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Web router assembly: API routes + static file serving.

pub mod api;
pub mod auth;

use axum::{middleware, routing::delete, routing::get, routing::post, Router};
use tower_http::cors::CorsLayer;

use api::ApiState;

/// Build the Axum router with all routes.
pub fn build_router(state: ApiState, auth_token: String) -> Router {
    let api = Router::new()
        .route("/status", get(api::get_status))
        .route("/families", get(api::list_families))
        .route(
            "/experiments",
            get(api::list_experiments).post(api::create_experiment),
        )
        .route("/experiments/:id", get(api::get_experiment))
        .route("/experiments/:id/logs", get(api::get_experiment_logs))
        .route("/experiments/:id/abort", post(api::abort_experiment))
        .route("/experiments/:id/cancel", post(api::cancel_experiment))
        .route("/experiments/:id/recreate", post(api::recreate_experiment))
        .route("/results/:id", get(api::download_results))
        .route("/results/:id/csv", get(api::download_results_csv))
        .route("/logs/server", get(api::get_server_logs))
        .route("/logs/:run_id", get(api::get_run_logs))
        .route("/events", get(api::sse_events))
        .route("/images", get(api::list_images))
        .route("/images/:id/download", post(api::start_image_download))
        .route("/images/:id", delete(api::evict_image))
        .layer(middleware::from_fn_with_state(
            auth_token.clone(),
            auth::auth_middleware,
        ))
        .with_state(state.clone());

    Router::new()
        .nest("/api", api)
        .fallback(serve_index)
        .layer(CorsLayer::permissive())
}

async fn serve_index() -> impl axum::response::IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("../../static/index.html"),
    )
}
