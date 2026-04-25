/// A scored correspondence between a path in the base image and a path in the
/// target image.
#[derive(Debug, Clone)]
pub struct PathMatch {
    pub base_path: String,
    pub target_path: String,
    /// Similarity score in `[0.0, 1.0]`.  Higher is a better match.
    pub score: f64,
}

/// Tuning parameters for the path-matching algorithm.
#[derive(Debug, Clone)]
pub struct PathMatchConfig {
    /// Matches below this score are discarded.
    pub min_score: f64,
}

impl Default for PathMatchConfig {
    fn default() -> Self {
        Self { min_score: 0.5 }
    }
}

/// Find the best base-image path for each target-image path.
///
/// Returns one [`PathMatch`] per target path that has a sufficiently similar
/// base counterpart (score ≥ `config.min_score`).  Target paths with no good
/// match are omitted — those files will be stored without a delta base.
///
/// The algorithm is ported from `playground/find_best_path_match_rust/`.
///
/// # Errors
///
/// Currently infallible; returns `Ok` always.  The `Result` wrapper exists for
/// future extension (e.g. loading a pre-computed index from disk).
pub fn find_best_matches(
    _base_paths: &[String],
    _target_paths: &[String],
    _config: &PathMatchConfig,
) -> crate::Result<Vec<PathMatch>> {
    todo!("Phase 3: path similarity algorithm from playground/find_best_path_match_rust/")
}
