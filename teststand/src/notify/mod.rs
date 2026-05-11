// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Telegram notification manager for experiment lifecycle events.

use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::{
    config::{experiment::ExperimentSpec, TelegramConfig},
    db::{self, Db},
    runner::queue::Queue,
};

pub struct NotifyManager {
    telegram: Option<TelegramConfig>,
    db: Db,
    queue: Queue,
    client: reqwest::Client,
}

impl NotifyManager {
    pub fn new(telegram: Option<TelegramConfig>, db: Db, queue: Queue) -> Arc<Self> {
        Arc::new(Self {
            telegram,
            db,
            queue,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        })
    }

    /// Collect all subscriber chat IDs: config list + DB-registered ones.
    async fn all_subscribers(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .telegram
            .as_ref()
            .map(|t| t.subscribers.clone())
            .unwrap_or_default();

        match db::list_telegram_subscribers(&self.db).await {
            Ok(db_ids) => {
                for id in db_ids {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
            Err(e) => warn!(err = %e, "failed to load telegram subscribers from DB"),
        }
        ids
    }

    /// Send a plain-text / HTML notification to all subscribers.
    /// Logs a warning on failure but never panics.
    pub async fn send(&self, message: &str) {
        let Some(tg) = &self.telegram else { return };
        let url = format!("https://api.telegram.org/bot{}/sendMessage", tg.bot_token);
        let subscribers = self.all_subscribers().await;

        for chat_id in subscribers {
            match self
                .client
                .post(&url)
                .json(&serde_json::json!({
                    "chat_id": chat_id,
                    "text": message,
                    "parse_mode": "HTML"
                }))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => info!(chat_id, "telegram notification sent"),
                Ok(r) => warn!(chat_id, status = %r.status(), "telegram notification failed"),
                Err(e) => warn!(chat_id, err = %e, "telegram notification error"),
            }
        }
    }

    /// Send a raw message to a single chat_id (used by the bot reply).
    async fn reply(&self, bot_token: &str, chat_id: i64, text: &str) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", bot_token);
        let _ = self
            .client
            .post(&url)
            .json(&serde_json::json!({"chat_id": chat_id, "text": text, "parse_mode": "HTML"}))
            .send()
            .await;
    }

    /// Build /status reply text.
    async fn build_status_text(&self) -> String {
        let mut out = String::from("<b>🖥 Teststand status</b>\n");
        let now = db::now_ts();

        match db::list_experiments(&self.db).await {
            Ok(exps) => {
                let running: Vec<_> = exps.iter().filter(|e| e.status == "running").collect();
                let done_exps: Vec<_> = exps
                    .iter()
                    .filter(|e| e.status == "done" || e.status == "error")
                    .take(5)
                    .collect();

                if running.is_empty() {
                    out.push_str("\n<b>▶ Running:</b> none\n");
                } else {
                    out.push_str("\n<b>▶ Running:</b>\n");
                    for exp in &running {
                        let results = db::get_results_for_experiment(&self.db, &exp.id)
                            .await
                            .unwrap_or_default();

                        let done_runs = results.len();
                        let elapsed_secs = (now - exp.created_at).max(0) as f64;

                        // Parse spec to know total expected runs.
                        let spec: Option<ExperimentSpec> =
                            serde_json::from_str(&exp.spec_json).ok();

                        let total_runs: Option<usize> = spec.as_ref().and_then(|s| {
                            s.images.as_ref().map(|imgs| {
                                let targets = imgs.len().saturating_sub(1);
                                targets * s.workers.len() * s.runs_per_pair
                            })
                        });

                        // Progress string: "3/12 (25%)" or "3/?"
                        let progress_str = match total_runs {
                            Some(total) if total > 0 => {
                                let pct = done_runs * 100 / total;
                                format!("{done_runs}/{total} ({pct}%)")
                            }
                            _ => format!("{done_runs}/?"),
                        };

                        // ETA: rate-based, only if we have enough data.
                        let eta_str = if done_runs > 0 && elapsed_secs > 1.0 {
                            let secs_per_run = elapsed_secs / done_runs as f64;
                            match total_runs {
                                Some(total) if total > done_runs => {
                                    let remaining = (total - done_runs) as f64 * secs_per_run;
                                    format!(", ETA ~{}", fmt_duration(remaining))
                                }
                                _ => {
                                    // No total — just show rate.
                                    format!(", ~{}/run", fmt_duration(secs_per_run))
                                }
                            }
                        } else {
                            String::new()
                        };

                        let elapsed_str = fmt_duration(elapsed_secs);

                        // Brief C/C* stats from completed results.
                        let stats_str = if done_runs > 0 {
                            let c_vals: Vec<f64> = results.iter().filter_map(|r| r.c).collect();
                            let cs_vals: Vec<f64> =
                                results.iter().filter_map(|r| r.cstar).collect();
                            let avg = |v: &[f64]| {
                                if v.is_empty() {
                                    None
                                } else {
                                    Some(v.iter().sum::<f64>() / v.len() as f64)
                                }
                            };
                            match (avg(&c_vals), avg(&cs_vals)) {
                                (Some(c), Some(cs)) => {
                                    format!(
                                        "\n    C: <code>{c:.3}</code>  C*: <code>{cs:.3}</code>"
                                    )
                                }
                                _ => String::new(),
                            }
                        } else {
                            String::new()
                        };

                        out.push_str(&format!(
                            "  • <code>{}</code> — {progress_str} ⏱ {elapsed_str}{eta_str}{stats_str}\n",
                            exp.name
                        ));
                    }
                }

                // Queued items (in-memory).
                let queued = self.queue.list_items().await;
                if queued.is_empty() {
                    out.push_str("\n<b>⏳ Queue:</b> empty\n");
                } else {
                    out.push_str(&format!("\n<b>⏳ Queue ({}):</b>\n", queued.len()));
                    for item in &queued {
                        out.push_str(&format!("  • <code>{}</code>\n", item.spec.name));
                    }
                }

                // Recent completions with relative time.
                if !done_exps.is_empty() {
                    out.push_str("\n<b>✅ Recent:</b>\n");
                    for exp in done_exps {
                        let icon = if exp.status == "done" { "✅" } else { "❌" };
                        let age_secs =
                            (now - exp.finished_at.unwrap_or(exp.created_at)).max(0) as f64;
                        let age_str = fmt_duration(age_secs);
                        out.push_str(&format!(
                            "  {icon} <code>{}</code> — {age_str} ago\n",
                            exp.name
                        ));
                    }
                }
            }
            Err(e) => {
                out.push_str(&format!("\nDB error: {e}"));
            }
        }

        out
    }

    /// Spawn a long-polling task that handles bot commands.
    pub fn start_bot_polling(self: Arc<Self>) {
        let Some(tg) = self.telegram.clone() else {
            return;
        };
        let mgr = Arc::clone(&self);
        tokio::spawn(async move {
            mgr.bot_loop(&tg.bot_token).await;
        });
        info!("telegram bot polling started");
    }

    async fn bot_loop(&self, bot_token: &str) {
        let get_updates_url = format!("https://api.telegram.org/bot{}/getUpdates", bot_token);
        let mut offset: i64 = 0;

        loop {
            let result = self
                .client
                .get(&get_updates_url)
                .timeout(Duration::from_secs(45))
                .query(&[
                    ("offset", offset.to_string()),
                    ("timeout", "30".into()),
                    ("allowed_updates", r#"["message"]"#.to_string()),
                ])
                .send()
                .await;

            let body = match result {
                Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(err = %e, "telegram getUpdates parse error");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                },
                Ok(r) => {
                    warn!(status = %r.status(), "telegram getUpdates non-200");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
                Err(e) => {
                    warn!(err = %e, "telegram getUpdates error, retry in 10s");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
            };

            let Some(updates) = body["result"].as_array() else {
                continue;
            };

            for update in updates {
                let update_id = update["update_id"].as_i64().unwrap_or(0);
                offset = offset.max(update_id + 1);

                let Some(msg) = update.get("message") else {
                    continue;
                };
                let chat_id = msg["chat"]["id"].as_i64().unwrap_or(0);
                let text = msg["text"].as_str().unwrap_or("").trim();

                if text.starts_with("/start") || text.starts_with("/subscribe") {
                    match db::add_telegram_subscriber(&self.db, chat_id).await {
                        Ok(_) => {
                            info!(chat_id, "new telegram subscriber");
                            self.reply(
                                bot_token,
                                chat_id,
                                "✅ Subscribed! You will receive teststand notifications.",
                            )
                            .await;
                        }
                        Err(e) => warn!(chat_id, err = %e, "subscribe failed"),
                    }
                } else if text.starts_with("/unsubscribe") {
                    match db::remove_telegram_subscriber(&self.db, chat_id).await {
                        Ok(_) => {
                            info!(chat_id, "telegram subscriber removed");
                            self.reply(bot_token, chat_id, "👋 Unsubscribed.").await;
                        }
                        Err(e) => warn!(chat_id, err = %e, "unsubscribe failed"),
                    }
                } else if text.starts_with("/status") {
                    let status_text = self.build_status_text().await;
                    self.reply(bot_token, chat_id, &status_text).await;
                } else {
                    self.reply(
                        bot_token,
                        chat_id,
                        "Commands: /status, /subscribe, /unsubscribe",
                    )
                    .await;
                }
            }
        }
    }
}

// ── Formatting helpers (public for runner use) ────────────────────────────────

pub fn fmt_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{:.0}s", secs)
    } else if secs < 3600.0 {
        format!("{:.0}m {:.0}s", secs / 60.0, secs % 60.0)
    } else {
        format!("{:.1}h", secs / 3600.0)
    }
}

/// Build a completion summary suitable for a Telegram HTML message.
pub fn fmt_completion_summary(
    exp_name: &str,
    status: &str,
    elapsed_secs: f64,
    total_runs: usize,
    total_target_mb: f64,
    total_archive_mb: f64,
    storage_peak_mb: f64,
    c_agg: f64,
    cstar_agg: f64,
) -> String {
    let icon = if status == "done" { "✅" } else { "❌" };
    format!(
        "{icon} <b>Experiment '{exp_name}' {status}</b> — {}\n\
         \n\
         <b>Runs</b>         {total_runs}\n\
         <b>Duration</b>     {}\n\
         <b>Total Target</b> {total_target_mb:.1} MB\n\
         <b>Total Archive</b>{total_archive_mb:.1} MB\n\
         <b>Storage peak</b> {storage_peak_mb:.1} MB\n\
         <b>C  (agg)</b>     {c_agg:.2}\n\
         <b>C* (agg)</b>     {cstar_agg:.2}",
        if status == "done" {
            "completed".to_owned()
        } else {
            format!("FAILED")
        },
        fmt_duration(elapsed_secs),
    )
}
