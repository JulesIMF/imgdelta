-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 JulesIMF
--
-- image-delta — incremental disk-image compression toolkit
-- 001_initial.sql — see module docs

-- Teststand SQLite schema

CREATE TABLE IF NOT EXISTS experiments (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    family      TEXT NOT NULL,
    kind        TEXT NOT NULL,
    spec_json   TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'queued',
    created_at  INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS runs (
    id            TEXT PRIMARY KEY,
    experiment_id TEXT NOT NULL REFERENCES experiments(id),
    run_index     INTEGER NOT NULL,
    workers       INTEGER NOT NULL,
    phase         TEXT NOT NULL,            -- "compress" | "decompress"
    status        TEXT NOT NULL DEFAULT 'pending', -- pending | running | done | error
    started_at    INTEGER,
    finished_at   INTEGER,
    error         TEXT
);

CREATE TABLE IF NOT EXISTS results (
    id                    TEXT PRIMARY KEY,
    run_id                TEXT NOT NULL REFERENCES runs(id),
    image_id              TEXT NOT NULL,
    base_image_id         TEXT,
    compress_stats_json   TEXT,
    decompress_stats_json TEXT,
    timing_json           TEXT,
    archive_bytes         INTEGER,
    base_qcow2_bytes      INTEGER,
    cstar                 REAL
);

CREATE TABLE IF NOT EXISTS log_lines (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id  TEXT NOT NULL,
    level   TEXT NOT NULL,
    ts      INTEGER NOT NULL,
    message TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_runs_experiment ON runs(experiment_id);
CREATE INDEX IF NOT EXISTS idx_results_run ON results(run_id);
CREATE INDEX IF NOT EXISTS idx_log_run ON log_lines(run_id);
