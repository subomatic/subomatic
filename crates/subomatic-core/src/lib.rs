// SPDX-License-Identifier: Apache-2.0
//! `subomatic-core` — timing-only subtitle synchronization.
//!
//! This crate is deliberately free of any audio-decode or platform code. It
//! takes already-decoded inputs (a subtitle plus a reference *activity signal*,
//! both expressed as time [`Span`]s) and warps the subtitle's timings to match.
//! Keeping it pure (`#![forbid(unsafe_code)]`, no platform deps) lets it compile
//! unchanged to native libraries and to WASM.
//!
//! Layers:
//! - [`cue`] — the format-agnostic subtitle model ([`Cue`], [`Subtitle`]).
//! - [`srt`] — SubRip parse/serialize (the first Tier-1 format).
//! - [`align`] — the alignment engine that finds the timing correction.
//!
//! [`sync`] is the high-level entry point: subtitle + reference spans in, a
//! re-timed subtitle out.

#![forbid(unsafe_code)]

pub mod align;
pub mod ass;
mod convert;
pub mod cue;
pub mod microdvd;
pub mod srt;
mod text;
pub mod vad;
pub mod vtt;

/// Exact NTSC film frame rate (24000/1001 ≈ 23.976) — the single source for both
/// the MicroDVD fallback rate and the alignment engine's play-rate scan.
pub(crate) const NTSC_FILM_FPS: f64 = 24_000.0 / 1_001.0;

pub use align::{
    best_alignment, best_alignment_with_progress, AlignParams, Alignment, SearchRange,
};
pub use cue::{Cue, Format, ParseError, Subtitle};
#[cfg(feature = "earshot")]
pub use vad::EarshotVad;
pub use vad::{EnergyVad, Vad};

/// A half-open time interval in integer milliseconds: `[start, end)`.
///
/// Used both for subtitle cues and for reference voice-activity spans, since the
/// alignment engine treats every input as a set of intervals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: i64,
    pub end: i64,
}

impl Span {
    /// Create a span. If `end <= start` the span is considered empty.
    pub fn new(start: i64, end: i64) -> Self {
        Span { start, end }
    }

    /// Duration in milliseconds (0 if the span is empty).
    pub fn len(&self) -> i64 {
        self.end.saturating_sub(self.start).max(0)
    }

    /// Whether the span covers no time.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// Length of the overlap between two spans, in milliseconds (0 if disjoint).
    pub fn overlap(&self, other: &Span) -> i64 {
        self.end
            .min(other.end)
            .saturating_sub(self.start.max(other.start))
            .max(0)
    }

    /// This span moved later by `delta` ms (earlier if `delta` is negative).
    ///
    /// Saturating arithmetic keeps pathological times within `i64` bounds
    /// instead of overflowing.
    pub fn shifted(&self, delta: i64) -> Span {
        Span {
            start: self.start.saturating_add(delta),
            end: self.end.saturating_add(delta),
        }
    }
}

/// Synchronize `subtitle` to a reference activity signal (voice-activity spans,
/// or a known-good subtitle's cue spans), returning a new, re-timed subtitle.
///
/// Searches common frame-rate ratios and a per-cue offset alignment (see
/// [`best_alignment`]), then applies the resulting warp — leaving every cue's
/// payload untouched.
pub fn sync(subtitle: &Subtitle, reference: &[Span], params: &AlignParams) -> Subtitle {
    sync_with_progress(subtitle, reference, params, &mut |_| {})
}

/// [`sync`] that reports alignment progress as a fraction in `0.0..=1.0`, so a
/// front-end can show a progress bar over the (dominant) alignment search. The
/// reference spans must already be computed — voice-activity detection, when
/// used, reports its own progress separately (see [`Vad::detect_with_progress`]).
pub fn sync_with_progress(
    subtitle: &Subtitle,
    reference: &[Span],
    params: &AlignParams,
    progress: &mut dyn FnMut(f64),
) -> Subtitle {
    let cues = subtitle.spans();
    let alignment = align::best_alignment_with_progress(reference, &cues, params, progress);
    let mut out = subtitle.clone();
    out.apply_alignment(&alignment);
    out
}
