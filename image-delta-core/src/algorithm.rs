use num_enum::TryFromPrimitive;
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// One-byte code identifying the file-level patch algorithm stored in a [`PatchRef`].
///
/// Using a fixed-width code instead of a string saves 6–15 bytes per manifest
/// entry.  [`AlgorithmCode::Extended`] (`0xFF`) signals that the numeric code
/// space is exhausted and the string `algorithm_id` field in [`PatchRef`] must
/// be consulted instead.  No built-in algorithm currently uses `Extended`.
///
/// Values `0x03..=0xFE` are reserved for future built-in algorithms.
///
/// [`PatchRef`]: crate::PatchRef
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, TryFromPrimitive)]
#[repr(u8)]
pub enum AlgorithmCode {
    /// No compression — target bytes stored verbatim (`"passthrough"`).
    Passthrough = 0x00,
    /// VCDIFF encoding via xdelta3 (`"xdelta3"`).
    Xdelta3 = 0x01,
    /// Myers line-level diff via the `diffy` crate (`"text-diff"`).
    TextDiff = 0x02,
    // 0x03..=0xFE reserved for future built-in algorithms
    /// Code space exhausted — consult the string `algorithm_id` in [`PatchRef`].
    Extended = 0xFF,
}

impl AlgorithmCode {
    /// Return the raw byte value of this code.
    #[inline]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

impl Serialize for AlgorithmCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.as_u8())
    }
}

impl<'de> Deserialize<'de> for AlgorithmCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = u8::deserialize(d)?;
        Self::try_from(v)
            .map_err(|e| de::Error::custom(format!("unknown algorithm code: {:#04x}", e.number)))
    }
}

// ── FileSnapshot ──────────────────────────────────────────────────────────────

/// A snapshot of a single file: its identity metadata and raw bytes.
///
/// [`PatchEncoder::encode`] receives **two** `FileSnapshot`s — one for the
/// base version and one for the target version — so both sides are described
/// symmetrically with the same struct.
///
/// Routing encoders (e.g. [`RouterEncoder`]) use `path`, `size`, and `header`
/// from the **target** snapshot to select the right algorithm.  Concrete
/// encoders (e.g. [`Xdelta3Encoder`]) use only the `bytes` field from each
/// snapshot and ignore the routing fields.
///
/// [`PatchEncoder::encode`]: crate::PatchEncoder::encode
/// [`RouterEncoder`]: crate::RouterEncoder
/// [`Xdelta3Encoder`]: crate::Xdelta3Encoder
pub struct FileSnapshot<'a> {
    /// File path relative to the image root, forward-slash separated.
    ///
    /// Used by glob-based routing rules.
    pub path: &'a str,
    /// Uncompressed file size in bytes.
    ///
    /// Used by size-based routing rules.
    pub size: u64,
    /// First bytes of the file content (up to 16 bytes), for magic/ELF detection.
    ///
    /// Used by magic-byte and ELF routing rules.
    pub header: &'a [u8],
    /// Raw file content.
    pub bytes: &'a [u8],
}

// ── FilePatch ─────────────────────────────────────────────────────────────────

/// The result of [`PatchEncoder::encode`]: patch bytes together with the
/// algorithm that produced them.
///
/// Bundling the algorithm with the bytes means callers—including
/// [`RouterEncoder`]—can build a correct [`PatchRef`] without a second call
/// to the encoder that was actually selected.
///
/// [`PatchEncoder::encode`]: crate::PatchEncoder::encode
/// [`RouterEncoder`]: crate::RouterEncoder
/// [`PatchRef`]: crate::PatchRef
#[derive(Debug)]
pub struct FilePatch {
    /// Raw patch bytes to archive and later pass to [`PatchEncoder::decode`].
    ///
    /// [`PatchEncoder::decode`]: crate::PatchEncoder::decode
    pub bytes: Vec<u8>,
    /// Compact one-byte algorithm code.
    pub code: AlgorithmCode,
    /// Human-readable algorithm id — `Some` only when
    /// `code == AlgorithmCode::Extended`.
    pub algorithm_id: Option<String>,
}

impl FilePatch {
    /// Construct a `FilePatch` for a built-in algorithm (code ≠ `Extended`).
    ///
    /// # Panics (debug only)
    ///
    /// Debug-asserts that `code != AlgorithmCode::Extended`.
    /// Use [`FilePatch::extended`] for extended algorithms.
    pub fn new(bytes: Vec<u8>, code: AlgorithmCode) -> Self {
        debug_assert_ne!(
            code,
            AlgorithmCode::Extended,
            "use FilePatch::extended for extended algorithms"
        );
        Self {
            bytes,
            code,
            algorithm_id: None,
        }
    }

    /// Construct a `FilePatch` for an extended (non-built-in) algorithm.
    pub fn extended(bytes: Vec<u8>, algorithm_id: impl Into<String>) -> Self {
        Self {
            bytes,
            code: AlgorithmCode::Extended,
            algorithm_id: Some(algorithm_id.into()),
        }
    }
}
