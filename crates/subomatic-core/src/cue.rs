// SPDX-License-Identifier: Apache-2.0
//! The format-agnostic subtitle model.
//!
//! Synchronization is timing-only, so a subtitle is just an ordered list of
//! cues, each a time interval plus an *opaque* payload (text, styling, …) that
//! the engine never inspects and never alters — only `start_ms`/`end_ms` move.

use crate::align::{scale_time, Alignment};
use crate::Span;

/// Subtitle container formats Subomatic round-trips.
///
/// Re-serializing always emits the *same* format the subtitle was parsed from,
/// so styling and other payload survive untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// SubRip (`.srt`).
    SubRip,
    /// WebVTT (`.vtt`).
    WebVtt,
    /// MicroDVD (`.sub`), frame-based.
    MicroDvd,
    /// Advanced SubStation Alpha (`.ass`/`.ssa`).
    Ass,
}

/// One subtitle entry: a time interval plus its original, untouched payload
/// (the cue's text/body, verbatim — the engine only moves the timing).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    /// The cue's original text/markup, preserved verbatim across a round-trip.
    pub payload: String,
}

impl Cue {
    /// Convenience constructor.
    pub fn new(start_ms: i64, end_ms: i64, payload: impl Into<String>) -> Self {
        Cue {
            start_ms,
            end_ms,
            payload: payload.into(),
        }
    }

    /// This cue's time interval, as seen by the alignment engine.
    pub fn span(&self) -> Span {
        Span::new(self.start_ms, self.end_ms)
    }
}

/// A parsed subtitle: its source format, an opaque format preamble (e.g. the
/// `WEBVTT` block — empty for SubRip/MicroDVD), and its ordered cues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subtitle {
    pub format: Format,
    pub header: String,
    pub cues: Vec<Cue>,
}

impl Subtitle {
    /// The cues' time intervals, in order — the engine's view of the subtitle.
    pub fn spans(&self) -> Vec<Span> {
        self.cues.iter().map(Cue::span).collect()
    }

    /// Shift every cue by `delta_ms` (negative moves earlier). This is how a
    /// global alignment result is applied back to the subtitle. Saturating
    /// arithmetic keeps pathological times within `i64` bounds.
    pub fn shift_all(&mut self, delta_ms: i64) {
        for cue in &mut self.cues {
            cue.start_ms = cue.start_ms.saturating_add(delta_ms);
            cue.end_ms = cue.end_ms.saturating_add(delta_ms);
        }
    }

    /// Apply an [`Alignment`] in place: scale every cue by `fps_ratio`, then add
    /// its per-cue offset (saturating). Cues without a matching offset keep the
    /// scaled time. Payloads are never touched.
    pub fn apply_alignment(&mut self, alignment: &Alignment) {
        for (i, cue) in self.cues.iter_mut().enumerate() {
            let offset = alignment.offsets.get(i).copied().unwrap_or(0);
            cue.start_ms = scale_time(cue.start_ms, alignment.fps_ratio).saturating_add(offset);
            cue.end_ms = scale_time(cue.end_ms, alignment.fps_ratio).saturating_add(offset);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_all_saturates_instead_of_overflowing() {
        let mut sub = Subtitle {
            format: Format::SubRip,
            header: String::new(),
            cues: vec![Cue::new(i64::MAX - 10, i64::MAX - 5, "x")],
        };
        sub.shift_all(1_000); // would overflow without saturation
        assert_eq!(sub.cues[0].start_ms, i64::MAX);
        assert_eq!(sub.cues[0].end_ms, i64::MAX);
    }

    #[test]
    fn apply_alignment_scales_then_offsets_and_keeps_payload() {
        let mut sub = Subtitle {
            format: Format::SubRip,
            header: String::new(),
            cues: vec![Cue::new(1_000, 2_000, "a"), Cue::new(10_000, 11_000, "b")],
        };
        let alignment = Alignment {
            fps_ratio: 2.0,
            offsets: vec![100, -100],
            score: 0,
        };
        sub.apply_alignment(&alignment);
        assert_eq!(sub.cues[0].start_ms, 2_100);
        assert_eq!(sub.cues[0].end_ms, 4_100);
        assert_eq!(sub.cues[1].start_ms, 19_900);
        assert_eq!(sub.cues[1].end_ms, 21_900);
        assert_eq!(sub.cues[0].payload, "a");
        assert_eq!(sub.cues[1].payload, "b");
    }
}
