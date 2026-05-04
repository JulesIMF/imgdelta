-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 JulesIMF
--
-- image-delta — incremental disk-image compression toolkit
-- Migration 0003: create the blob_index table (content-addressable blob registry)

-- CAS index: maps sha256 hex digest → deterministic blob UUID.
-- Used by S3Storage::blob_exists() and upload_blob() for deduplication.
CREATE TABLE IF NOT EXISTS blob_index (
    sha256 TEXT NOT NULL PRIMARY KEY,
    blob_id UUID NOT NULL UNIQUE
);