/// Integration tests for [`TextDiffEncoder`].
///
/// These tests exercise the full encode/decode cycle, as well as error paths
/// for binary inputs that are not valid UTF-8.
use image_delta_core::{DeltaEncoder, Error, TextDiffEncoder};

fn encoder() -> TextDiffEncoder {
    TextDiffEncoder::new()
}

// ── 1. Roundtrip: typical config file change ──────────────────────────────────

/// A simple, realistic text-file change round-trips correctly.
#[test]
fn test_text_diff_roundtrip_simple() {
    let source = b"[section]\nkey = value\nother = 1\n";
    let target = b"[section]\nkey = new_value\nother = 1\nextra = added\n";

    let enc = encoder();
    let delta = enc.encode(source, target).expect("encode must succeed");
    assert!(!delta.is_empty(), "delta must not be empty");

    let recovered = enc.decode(source, &delta).expect("decode must succeed");
    assert_eq!(
        recovered, target,
        "decoded output must match original target"
    );
}

// ── 2. Roundtrip: empty source (new file) ────────────────────────────────────

/// Encoding an empty source against a non-empty target must produce a valid patch
/// and decode back to the target.
#[test]
fn test_text_diff_roundtrip_empty_source() {
    let source = b"";
    let target = b"line one\nline two\nline three\n";

    let enc = encoder();
    let delta = enc.encode(source, target).expect("encode must succeed");
    let recovered = enc.decode(source, &delta).expect("decode must succeed");
    assert_eq!(recovered, target);
}

// ── 3. Roundtrip: empty target (file deletion equivalent) ────────────────────

/// Encoding a non-empty source against an empty target must produce a valid patch
/// and decode back to empty.
#[test]
fn test_text_diff_roundtrip_empty_target() {
    let source = b"will be deleted\nline two\n";
    let target = b"";

    let enc = encoder();
    let delta = enc.encode(source, target).expect("encode must succeed");
    let recovered = enc.decode(source, &delta).expect("decode must succeed");
    assert_eq!(recovered, target);
}

// ── 4. Roundtrip: identical files ─────────────────────────────────────────────

/// When source == target the encoder must still produce a valid (empty-change)
/// patch that round-trips to the same content.
#[test]
fn test_text_diff_roundtrip_identical() {
    let content = b"unchanged line\nanother unchanged line\n";

    let enc = encoder();
    let delta = enc
        .encode(content, content)
        .expect("encoding identical files must succeed");

    let recovered = enc
        .decode(content, &delta)
        .expect("decoding identical-file patch must succeed");
    assert_eq!(recovered, content);
}

// ── 5. Error: non-UTF-8 source ────────────────────────────────────────────────

/// `encode` must return `Error::Encode` when the source is not valid UTF-8.
#[test]
fn test_text_diff_encode_error_non_utf8_source() {
    let source: &[u8] = &[0xFF, 0xFE, 0x00]; // invalid UTF-8
    let target = b"valid utf-8 text\n";

    let enc = encoder();
    let err = enc
        .encode(source, target)
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

/// `encode` must return `Error::Encode` when the target is not valid UTF-8.
#[test]
fn test_text_diff_encode_error_non_utf8_target() {
    let source = b"valid utf-8 source\n";
    let target: &[u8] = &[0x80, 0x81, 0x82]; // invalid UTF-8

    let enc = encoder();
    let err = enc
        .encode(source, target)
        .expect_err("must fail on binary target");
    assert!(
        matches!(err, Error::Encode(_)),
        "expected Error::Encode, got {err:?}"
    );
}

// ── 7. Error: non-UTF-8 delta in decode ──────────────────────────────────────

/// `decode` must return `Error::Encode` when the delta bytes are not valid UTF-8.
#[test]
fn test_text_diff_decode_error_non_utf8_delta() {
    let source = b"some source text\n";
    let corrupt_delta: &[u8] = &[0xFF, 0xFE]; // not a valid UTF-8 diff

    let enc = encoder();
    let err = enc
        .decode(source, corrupt_delta)
        .expect_err("must fail on binary delta");
    assert!(
        matches!(err, Error::Encode(_)),
        "expected Error::Encode, got {err:?}"
    );
}

// ── 8. Error: patch applied to wrong source ───────────────────────────────────

/// `decode` must return `Error::Encode` when the patch cannot be applied to
/// the given source (context mismatch — wrong base).
#[test]
fn test_text_diff_decode_error_wrong_source() {
    let source_a = b"first line\nsecond line\nthird line\n";
    let source_b = b"first line\nTOTALLY DIFFERENT SECOND\nthird line\n";
    let target_a = b"first line\nsecond line MODIFIED\nthird line\n";

    let enc = encoder();
    // Build a patch from source_a → target_a.
    let delta = enc.encode(source_a, target_a).expect("encode must succeed");

    // Applying that patch to source_b (different context) must fail.
    let err = enc
        .decode(source_b, &delta)
        .expect_err("must fail when patch context doesn't match source");
    assert!(
        matches!(err, Error::Encode(_)),
        "expected Error::Encode, got {err:?}"
    );
}

// ── 9. Multiline change roundtrip ─────────────────────────────────────────────

/// Multiple insertions, deletions, and unchanged lines round-trip correctly.
#[test]
fn test_text_diff_roundtrip_multiline() {
    let source = "line 1\nline 2\nline 3\nline 4\nline 5\n";
    let target = "line 1\nLINE 2 changed\nline 3\nINSERTED\nline 5\n";

    let enc = encoder();
    let delta = enc
        .encode(source.as_bytes(), target.as_bytes())
        .expect("encode must succeed");
    let recovered = enc
        .decode(source.as_bytes(), &delta)
        .expect("decode must succeed");

    assert_eq!(
        std::str::from_utf8(&recovered).unwrap(),
        target,
        "multiline roundtrip failed"
    );
}

// ── 10. Algorithm ID ──────────────────────────────────────────────────────────

#[test]
fn test_text_diff_algorithm_id() {
    assert_eq!(encoder().algorithm_id(), "text-diff");
}
