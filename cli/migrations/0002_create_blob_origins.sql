-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 JulesIMF
--
-- image-delta — incremental disk-image compression toolkit
-- Migration 0002: create the blob_origins table (sha256 → image mapping)

CREATE TABLE IF NOT EXISTS blob_origins (
    blob_id       UUID        NOT NULL,
    orig_image_id TEXT        NOT NULL REFERENCES images(image_id),
    base_image_id TEXT        REFERENCES images(image_id),
    path          TEXT        NOT NULL,
    size          BIGINT      NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (blob_id, orig_image_id)
);