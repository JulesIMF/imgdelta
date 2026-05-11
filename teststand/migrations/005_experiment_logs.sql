-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 Jules IMF
--
-- image-delta — incremental disk-image compression toolkit
-- 005_experiment_logs.sql — per-experiment log line storage.
CREATE TABLE experiment_log_lines (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    experiment_id TEXT NOT NULL,
    run_id TEXT,
    level TEXT NOT NULL,
    ts INTEGER NOT NULL,
    message TEXT NOT NULL
);
CREATE INDEX idx_exp_log_exp_id ON experiment_log_lines(experiment_id);
-- Add separate C column to results: C = base/archive (pure delta ratio).
-- The existing cstar column is repurposed as C* = (base+target)/(base+archive).
ALTER TABLE results
ADD COLUMN c REAL;