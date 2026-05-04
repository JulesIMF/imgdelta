-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 JulesIMF
--
-- image-delta — incremental disk-image compression toolkit
-- Migration 0001: create the images table (image_id, base_image_id, status, format)

CREATE TABLE IF NOT EXISTS images (
    image_id TEXT PRIMARY KEY,
    base_image_id TEXT,
    format TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    status_detail TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);