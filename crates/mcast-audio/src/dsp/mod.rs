//! Audio processing blocks used by [`crate::process`].
//!
//! All blocks work on interleaved f32 samples at the source's sample rate.

pub mod biquad;
pub mod deesser;
pub mod denoise;
pub mod dynamics;
pub mod loudness;

/// dB to linear amplitude.
pub fn db_to_lin(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Linear amplitude to dB.
pub fn lin_to_db(lin: f64) -> f64 {
    20.0 * lin.log10()
}
