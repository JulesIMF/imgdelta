// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// teststand binary entry-point: wires up logging, DB, runner, and web server.

mod config;
mod db;
mod error;
mod image_manager;
mod logging;
mod notify;
mod runner;
mod web;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use clap::Parser;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::{
    config::load_config,
    config::load_families,
    error::Result,
    image_manager::ImageManager,
    logging::{BufferLayer, LogBuffer, SseBroadcastLayer},
    notify::NotifyManager,
    runner::progress,
    runner::queue::Queue,
    runner::Runner,
    web::{api::ApiState, build_router},
};

#[derive(Parser)]
#[command(name = "teststand", about = "imgdelta experiment orchestrator")]
struct Cli {
    /// Path to teststand.toml
    #[arg(short, long, default_value = "teststand.toml")]
    config: PathBuf,
    /// Path to families.toml or a directory of per-family TOML files
    #[arg(short, long, default_value = "families")]
    families: PathBuf,
    /// Override listen port (overrides config)
    #[arg(short, long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // ── Create the SSE broadcast channel FIRST so logging layers can use it ──
    let (progress_tx, _) = progress::channel();

    // ── Logging: fmt to stderr + ring-buffer + SSE broadcast ────────────────
    let log_buffer = LogBuffer::new(2000);

    let buffer_layer = BufferLayer {
        buffer: Arc::clone(&log_buffer),
    };
    let sse_layer = SseBroadcastLayer {
        tx: progress_tx.clone(),
    };

    let fmt_layer = fmt::layer().with_target(true).with_level(true);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("teststand=info,image_delta_core=info,warn"));

    tracing_subscriber::registry()
        .with(fmt_layer.with_filter(filter))
        .with(buffer_layer)
        .with(sse_layer)
        .init();

    let cli = Cli::parse();

    let cfg = Arc::new(load_config(&cli.config)?);
    let families = Arc::new(load_families(&cli.families)?);

    // Ensure base directories exist (per-experiment dirs are created by the runner)
    std::fs::create_dir_all(cfg.workdir.as_path())?;
    std::fs::create_dir_all(cfg.images_dir())?;
    std::fs::create_dir_all(cfg.workdir.join("experiments"))?;

    // Database
    let db = db::open(&cfg.db_path()).await?;

    // Image manager
    let image_manager = ImageManager::new(cfg.images_dir());
    for family in &families.families {
        image_manager.register(family.images.clone()).await;
    }

    // Shared run queue (passed to both Runner and NotifyManager)
    let runner_queue = Queue::new();

    // Notifications
    let notify = NotifyManager::new(cfg.telegram.clone(), db.clone(), runner_queue.clone());

    // Runner
    let runner = Runner::new(
        runner_queue,
        Arc::clone(&cfg),
        db.clone(),
        Arc::clone(&families),
        Arc::clone(&image_manager),
        progress_tx.clone(),
        Arc::clone(&notify),
    );
    Arc::clone(&runner).start();

    // Start Telegram bot polling (no-op if telegram not configured)
    Arc::clone(&notify).start_bot_polling();

    // Send startup notification (non-fatal)
    let port = cli.port.unwrap_or(cfg.port);
    {
        let n = Arc::clone(&notify);
        let msg = format!("\u{1F680} teststand started on port {port}");
        tokio::spawn(async move {
            n.send(&msg).await;
        });
    }

    // Web
    let api_state = ApiState {
        db: db.clone(),
        runner: Arc::clone(&runner),
        progress_tx: progress_tx.clone(),
        log_buffer: Arc::clone(&log_buffer),
        families: Arc::clone(&families),
    };
    let app = build_router(api_state, cfg.auth_token.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(addr = %addr, "teststand listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
