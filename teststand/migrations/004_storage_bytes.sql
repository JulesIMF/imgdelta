-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 Jules IMF
--
-- image-delta — incremental disk-image compression toolkit
-- 004_storage_bytes.sql — per-experiment isolated storage; record total size.
ALTER TABLE results
ADD COLUMN storage_bytes INTEGER;