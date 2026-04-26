/// Integration tests for [`TextDiffEncoder`].
///
/// These tests exercise the full encode/decode cycle, as well as error paths
/// for binary inputs that are not valid UTF-8.
use image_delta_core::{
    AlgorithmCode, Error, FilePatch, FileSnapshot, PatchEncoder, TextDiffEncoder,
};

fn encoder() -> TextDiffEncoder {
    TextDiffEncoder::new()
}

/// Helper: build a minimal [`FileSnapshot`] from raw bytes.
fn snap<'a>(bytes: &'a [u8]) -> FileSnapshot<'a> {
    FileSnapshot {
        bytes,
        path: "",
        size: bytes.len() as u64,
        header: &bytes[..bytes.len().min(16)],
    }
}

// ── 1. Roundtrip: typical config file change ──────────────────────────────────

/// A simple, realistic text-file change round-trips correctly.
#[test]
fn test_text_diff_roundtrip_simple() {
    let source = b"[section]\nkey = value\nother = 1\n";
    let target = b"[section]\nkey = new_value\nother = 1\nextra = added\n";

    let enc = encoder();
    let patch = enc
        .encode(&snap(source), &snap(target))
        .expect("encode must succeed");
    assert!(!patch.bytes.is_empty(), "patch must not be empty");

    let recovered = enc.decode(source, &patch).expect("decode must succeed");
    assert_eq!(
        recovered, target,
        "decoded output must match original target"
    );
}

// ── 2. Roundtrip: empty source (new file) ────────────────────────────────────

#[test]
fn test_text_diff_roundtrip_empty_source() {
    let source = b"";
    let target = b"line one\nline two\nline three\n";

    let enc = encoder();
    let patch = enc
        .encode(&snap(source), &snap(target))
        .expect("encode must succeed");
    let recovered = enc.decode(source, &patch).expect("decode must succeed");
    assert_eq!(recovered, target);
}

// ── 3. Roundtrip: empty target ────────────────────────────────────────────────

#[test]
fn test_text_diff_roundtrip_empty_target() {
    let source = b"will be deleted\nline two\n";
    let target = b"";

    let enc = encoder();
    let patch = enc
        .encode(&snap(source), &snap(target))
        .expect("encode must succeed");
    let recovered = enc.decode(source, &patch).expect("decode must succeed");
    assert_eq!(recovered, target);
}

// ── 4. Roundtrip: identical files ─────────────────────────────────────────────

#[test]
fn test_text_diff_roundtrip_identical() {
    let content = b"unchanged line\nanother unchanged line\n";

    let enc = encoder();
    let patch = enc
        .encode(&snap(content), &snap(content))
        .expect("encoding identical files must succeed");
    let recovered = enc
        .decode(content, &patch)
        .expect("decoding identical-file patch must succeed");
    assert_eq!(recovered, content);
}

// ── 5. Error: non-UTF-8 source ────────────────────────────────────────────────

#[test]
fn test_text_diff_encode_error_non_utf8_source() {
    let source: &[u8] = &[0xFF, 0xFE, 0x00]; // invalid UTF-8
    let target = b"valid utf-8 text\n";

    let enc = encoder();
    let err = enc
        .encode(&snap(source), &snap(target))
        .expect_err("must fail on binary source");
    assert!(
        matches!(err, Error::Encode(_)),
        "expected Error::Encode, got {err:?}"
    );
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("utf-8") || msg.contains("utf8") || msg.contains("source"),
        "error message should mention UTF-8 or source: {msg}"
    );
}

// ── 6. Error: non-UTF-8 target ────────────────────────────────────────────────

#[test]
fn test_text_diff_encode_error_non_utf8_target() {
    let source = b"valid utf-8 source\n";
    let target: &[u8] = &[0x80, 0x81, 0x82]; // invalid UTF-8

    let enc = encoder();
    let err = enc
        .encode(&snap(source), &snap(target))
        .expect_err("must fail on binary target");
    assert!(
        matches!(err, Error::Encode(_)),
        "expected Error::Encode, got {err:?}"
    );
}

// ── 7. Error: non-UTF-8 patch in decode ──────────────────────────────────────

#[test]
fn test_text_diff_decode_error_non_utf8_delta() {
    let source = b"some source text\n";
    let corrupt_bytes: &[u8] = &[0xFF, 0xFE]; // not valid UTF-8
    let corrupt_patch = FilePatch::new(corrupt_bytes.to_vec(), AlgorithmCode::TextDiff);

    let enc = encoder();
    let err = enc
        .decode(source, &corrupt_patch)
        .expect_err("must fail on binary patch");
    assert!(
        matches!(err, Error::Decode(_)),
        "expected Error::Decode, got {err:?}"
    );
}

// ── 8. Error: patch applied to wrong source ───────────────────────────────────

#[test]
fn test_text_diff_decode_error_wrong_source() {
    let source_a = b"first line\nsecond line\nthird line\n";
    let source_b = b"first line\nTOTALLY DIFFERENT SECOND\nthird line\n";
    let target_a = b"first line\nsecond line MODIFIED\nthird line\n";

    let enc = encoder();
    let patch = enc
        .encode(&snap(source_a), &snap(target_a))
        .expect("encode must succeed");

    let err = enc
        .decode(source_b, &patch)
        .expect_err("must fail when patch context doesn't match source");
    assert!(
        matches!(err, Error::Decode(_)),
        "expected Error::Decode, got {err:?}"
    );
}

// ── 9. Multiline change roundtrip ─────────────────────────────────────────────

#[test]
fn test_text_diff_roundtrip_multiline() {
    let source = "line 1\nline 2\nline 3\nline 4\nline 5\n";
    let target = "line 1\nLINE 2 changed\nline 3\nINSERTED\nline 5\n";

    let enc = encoder();
    let patch = enc
        .encode(&snap(source.as_bytes()), &snap(target.as_bytes()))
        .expect("encode must succeed");
    let recovered = enc
        .decode(source.as_bytes(), &patch)
        .expect("decode must succeed");
    assert_eq!(std::str::from_utf8(&recovered).unwrap(), target);
}

// ── 10. Algorithm code and ID ─────────────────────────────────────────────────

#[test]
fn test_text_diff_algorithm_id() {
    assert_eq!(encoder().algorithm_id(), "text-diff");
}

#[test]
fn test_text_diff_algorithm_code() {
    assert_eq!(encoder().algorithm_code(), Some(AlgorithmCode::TextDiff));
    let patch = encoder().encode(&snap(b"a\n"), &snap(b"b\n")).unwrap();
    assert_eq!(patch.code, AlgorithmCode::TextDiff);
    assert_eq!(patch.algorithm_id, None);
}
