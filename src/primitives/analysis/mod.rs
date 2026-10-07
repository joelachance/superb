//! Analysis: measuring audio rather than making it.
//!
//! Meters (sample peak, true peak, RMS, loudness in LUFS), lining two
//! recordings up by their audio, and finding silence. All of it is math
//! over `f32` slices: no threads, no files, no devices, no feature flags,
//! so it runs anywhere the crate does and can be reused by an application
//! that is not a DAW. The one-shot functions allocate; the running meters
//! ([`meter::TruePeakMeter`], [`meter::Loudness`]) allocate when built and
//! never while fed, so a graph node could carry one.
//!
//! * [`meter`]: `peak`, `rms`, `true_peak` (4× oversampled, ITU-R BS.1770
//!   Annex 2), `Loudness` (K-weighted momentary, short-term and gated
//!   integrated LUFS, EBU R128), `normalize` (a buffer to a target
//!   loudness under a true-peak ceiling).
//! * [`offset`]: `estimate`, how many samples one recording lags another,
//!   by cross-correlation, with a confidence.
//! * [`silence`]: `stretches` under a threshold, and `trimmed`, the span
//!   that is not.

mod fft;
pub mod meter;
pub mod offset;
pub mod silence;
