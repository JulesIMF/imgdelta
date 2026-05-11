-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 JulesIMF
--
-- image-delta — incremental disk-image compression toolkit
-- 002_telegram.sql — see module docs

-- Dynamically registered Telegram subscribers (via /subscribe bot command).
CREATE TABLE IF NOT EXISTS telegram_subscribers (
    chat_id   INTEGER PRIMARY KEY,
    added_at  INTEGER NOT NULL
);
