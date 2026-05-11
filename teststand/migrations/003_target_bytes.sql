-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 Jules IMF
--
-- image-delta — incremental disk-image compression toolkit
-- 003_target_bytes.sql — add target_qcow2_bytes column to results.

ALTER TABLE results ADD COLUMN target_qcow2_bytes INTEGER;
