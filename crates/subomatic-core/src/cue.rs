// SPDX-License-Identifier: Apache-2.0
//! The format-agnostic subtitle model.
//!
//! Synchronization is timing-only, so a subtitle is just an ordered list of
//! cues, each a time interval plus an *opaque* payload (text, styling, …) that
//! the engine never inspects and never alters — only `start_ms`/`end_ms` move.

use crate::Span;

/// Subtitle container formats Subomatic round-trips.
///
/// Re-serializing always emits the *same* format the subtitle was parsed from,
/// so styling and other payload survive untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// SubRip (`.srt`).
    SubRip,
    // Planned Tier 1: SubStation Alpha (.ass/.ssa), WebVTT (.vtt), MicroDVD (.sub).
}

/// One subtitle entry: a time interval plus its original, untouched payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    /// The cue's original text/markup, preserved verbatim across a round-trip.
    pub payload: String,
}

impl Cue {
    /// This cue's time interval, as seen by the alignment engine.
    pub fn span(&self) -> Span {
        Span::new(self.start_ms, self.end_ms)
    }
}

/// A parsed subtitle: its source format plus its ordered cues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subtitle {
    pub format: Format,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_all_saturates_instead_of_overflowing() {
        let mut sub = Subtitle {
            format: Format::SubRip,
            cues: vec![Cue {
                start_ms: i64::MAX - 10,
                end_ms: i64::MAX - 5,
                payload: "x".into(),
            }],
        };
        sub.shift_all(1_000); // would overflow without saturation
        assert_eq!(sub.cues[0].start_ms, i64::MAX);
        assert_eq!(sub.cues[0].end_ms, i64::MAX);
    }
}
