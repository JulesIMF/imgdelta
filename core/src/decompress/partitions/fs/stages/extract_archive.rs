// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 JulesIMF
//
// image-delta — incremental disk-image compression toolkit
// Decompress stage 1: extract patches archive

use std::collections::HashMap;
use std::io::Read;

use async_trait::async_trait;

use crate::decompress::partitions::fs::context::DecompressContext;
use crate::decompress::partitions::fs::draft::DecompressDraft;
use crate::decompress::partitions::fs::stage::DecompressStage;
use crate::{Error, Result};

// ── Stage struct ──────────────────────────────────────────────────────────────

/// Stage 1: Extract the patches tar (or tar.gz) archive into `draft.patch_map`.
///
/// If `ctx.archive_bytes` is empty the patch map is left empty (fast path).
pub struct ExtractArchive;

#[async_trait]
impl DecompressStage for ExtractArchive {
    fn name(&self) -> &'static str {
        "extract_archive"
    }

    async fn run(
        &self,
        ctx: &DecompressContext,
        mut draft: DecompressDraft,
    ) -> Result<DecompressDraft> {
        if !ctx.archive_bytes.is_empty() {
            draft.patch_map = extract_archive_fn(&ctx.archive_bytes, ctx.patches_compressed)?;
        }
        Ok(draft)
    }
}

// ── Implementation ────────────────────────────────────────────────────────────

/// Extract a patches tar (or tar.gz) archive into a map of `entry_name → bytes`.
pub fn extract_archive_fn(
    archive_bytes: &[u8],
    compressed: bool,
) -> Result<HashMap<String, Vec<u8>>> {
    let mut map = HashMap::new();

    if compressed {
        let decoder = flate2::read::GzDecoder::new(archive_bytes);
        let mut ar = tar::Archive::new(decoder);
        for entry in ar
            .entries()
            .map_err(|e| Error::Archive(format!("tar entries: {e}")))?
        {
            let mut entry = entry.map_err(|e| Error::Archive(format!("tar entry: {e}")))?;
            let name = entry
                .path()
                .map_err(|e| Error::Archive(format!("tar entry path: {e}")))?
                .to_string_lossy()
                .into_owned();
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| Error::Archive(format!("tar entry read: {e}")))?;
            map.insert(name, bytes);
        }
    } else {
        let mut ar = tar::Archive::new(archive_bytes);
        for entry in ar
            .entries()
            .map_err(|e| Error::Archive(format!("tar entries: {e}")))?
        {
            let mut entry = entry.map_err(|e| Error::Archive(format!("tar entry: {e}")))?;
            let name = entry
                .path()
                .map_err(|e| Error::Archive(format!("tar entry path: {e}")))?
                .to_string_lossy()
                .into_owned();
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| Error::Archive(format!("tar entry read: {e}")))?;
            map.insert(name, bytes);
        }
    }

    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, name, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let tar = make_tar(entries);
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&tar).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn test_extract_archive_uncompressed() {
        let tar = make_tar(&[("000000.patch", b"data-a"), ("000001.patch", b"data-b")]);
        let map = extract_archive_fn(&tar, false).unwrap();
        assert_eq!(map["000000.patch"], b"data-a");
        assert_eq!(map["000001.patch"], b"data-b");
    }

    #[test]
    fn test_extract_archive_compressed() {
        let tar_gz = make_tar_gz(&[("abc.patch", b"hello")]);
        let map = extract_archive_fn(&tar_gz, true).unwrap();
        assert_eq!(map["abc.patch"], b"hello");
    }

    #[test]
    fn test_extract_archive_empty_tar() {
        let tar = make_tar(&[]);
        let map = extract_archive_fn(&tar, false).unwrap();
        assert!(map.is_empty());
    }
}
