pub mod passthrough;
pub mod text_diff;
pub mod vcdiff;

pub use passthrough::PassthroughEncoder;
pub use text_diff::TextDiffEncoder;
pub use vcdiff::Xdelta3Encoder;
