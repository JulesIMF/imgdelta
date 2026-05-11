-- SPDX-License-Identifier: MIT OR Apache-2.0
-- Copyright (c) 2026 Jules IMF
--
-- image-delta — incremental disk-image compression toolkit
-- 006_result_run_info.sql — add workers and run_repetition to results.
ALTER TABLE results
ADD COLUMN workers INTEGER NOT NULL DEFAULT 0;
ALTER TABLE results
ADD COLUMN run_repetition INTEGER NOT NULL DEFAULT 0;
ALTER TABLE results
ADD COLUMN runs_total INTEGER NOT NULL DEFAULT 1;