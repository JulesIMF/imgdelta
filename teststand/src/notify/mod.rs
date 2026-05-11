// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 Jules IMF
//
// image-delta — incremental disk-image compression toolkit
// Telegram notification manager for experiment lifecycle events.

use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

use crate::{
    config::TelegramConfig,
    db::{self, Db},
};

pub struct NotifyManager {
    telegram: Option<TelegramConfig>,
    db: Db,
    client: reqwest::Client,
}

impl NotifyManager {
    pub fn new(telegram: Option<TelegramConfig>, db: Db) -> Arc<Self> {
        Arc::new(Self {
            telegram,
            db,
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

    /// Send a plain-text notification to all subscribers.
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
            .json(&serde_json::json!({"chat_id": chat_id, "text": text}))
            .send()
            .await;
    }

    /// Spawn a long-polling task that handles bot commands.
    ///
    /// Handles `/start` and `/subscribe` (adds caller to DB).
    /// Handles `/unsubscribe` (removes caller from DB).
    ///
    /// Errors are logged; the loop never propagates panics to the Tokio runtime.
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
                .timeout(Duration::from_secs(45)) // must exceed polling timeout=30
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
                    self.reply(bot_token, chat_id, "🟢 teststand is running")
                        .await;
                }
            }
        }
    }
}
