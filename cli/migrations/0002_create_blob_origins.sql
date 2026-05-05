-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 JulesIMF
--
-- image-delta — incremental disk-image compression toolkit
-- Migration 0002: create the blob_origins table (sha256 → image mapping)

CREATE TABLE IF NOT EXISTS blob_origins (
    blob_id          UUID        NOT NULL,
    orig_image_id    TEXT        NOT NULL REFERENCES images(image_id),
    base_image_id    TEXT        REFERENCES images(image_id),
    -- 1-based partition number within the source image.
    -- NULL for single-partition (DirectoryImage) compress runs.
    partition_number SMALLINT,
    path             TEXT        NOT NULL,
    size             BIGINT      NOT NULL,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (blob_id, orig_image_id)
);

-- Fast lookup of same-partition blob candidates during compress.
CREATE INDEX IF NOT EXISTS blob_origins_orig_part_idx
    ON blob_origins (orig_image_id, partition_number);
