// SPDX-License-Identifier: Apache-2.0
//! The format-agnostic subtitle model.
//!
//! Synchronization is timing-only, so a subtitle is just an ordered list of
//! cues, each a time interval plus an *opaque* payload (text, styling, …) that
//! the engine never inspects and never alters — only `start_ms`/`end_ms` move.

use crate::align::{scale_time, Alignment};
use crate::ass::AssError;
use crate::srt::SrtError;
use crate::vtt::VttError;
use crate::Span;
use std::fmt;

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

impl Format {
    /// Match a file extension (case-insensitive, without the leading dot) to a
    /// format, or `None` if unrecognized. The single source for the
    /// extension→format mapping the CLI and WASM/JS front-ends share.
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.to_ascii_lowercase().as_str() {
            "srt" => Some(Format::SubRip),
            "vtt" => Some(Format::WebVtt),
            "sub" => Some(Format::MicroDvd),
            "ass" | "ssa" => Some(Format::Ass),
            _ => None,
        }
    }
}

/// An error from [`Subtitle::parse`], wrapping the originating format's error.
///
/// A thin wrapper rather than a merged enum: each format keeps its own error
/// type, and the wrapper just carries whichever one occurred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    /// A SubRip (`.srt`) parse error.
    Srt(SrtError),
    /// A WebVTT (`.vtt`) parse error.
    Vtt(VttError),
    /// An ASS/SSA (`.ass`/`.ssa`) parse error.
    Ass(AssError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Srt(e) => e.fmt(f),
            ParseError::Vtt(e) => e.fmt(f),
            ParseError::Ass(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::Srt(e) => Some(e),
            ParseError::Vtt(e) => Some(e),
            ParseError::Ass(e) => Some(e),
        }
    }
}

impl From<SrtError> for ParseError {
    fn from(e: SrtError) -> Self {
        ParseError::Srt(e)
    }
}

impl From<VttError> for ParseError {
    fn from(e: VttError) -> Self {
        ParseError::Vtt(e)
    }
}

impl From<AssError> for ParseError {
    fn from(e: AssError) -> Self {
        ParseError::Ass(e)
    }
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

    /// Parse `text` as the given [`Format`]. `fps` is used only by MicroDVD
    /// (frame-based); the other formats ignore it. The single source for the
    /// format→parser dispatch the CLI and WASM front-ends share; pairs with
    /// [`Subtitle::serialize`].
    pub fn parse(format: Format, text: &str, fps: f64) -> Result<Subtitle, ParseError> {
        Ok(match format {
            Format::SubRip => crate::srt::parse(text)?,
            Format::WebVtt => crate::vtt::parse(text)?,
            Format::MicroDvd => crate::microdvd::parse(text, fps),
            Format::Ass => crate::ass::parse(text)?,
        })
    }

    /// Serialize back to this subtitle's original [`Format`] (re-emitting the
    /// same format it was parsed from). `fps` is used only by MicroDVD, which is
    /// frame-based; the other formats ignore it.
    pub fn serialize(&self, fps: f64) -> String {
        match self.format {
            Format::SubRip => crate::srt::serialize(self),
            Format::WebVtt => crate::vtt::serialize(self),
            Format::MicroDvd => crate::microdvd::serialize(self, fps),
            Format::Ass => crate::ass::serialize(self),
        }
    }

    /// Serialize to a chosen `target` [`Format`], converting the cues' markup and
    /// header as needed. Identical to [`serialize`](Subtitle::serialize) when
    /// `target` is the source format; otherwise SubRip↔WebVTT keep their inline
    /// markup while crossings to/from MicroDVD or ASS flatten to plain text (see
    /// the `convert` module). `fps` is used only when emitting MicroDVD.
    pub fn serialize_as(&self, target: Format, fps: f64) -> String {
        if target == self.format {
            self.serialize(fps)
        } else {
            crate::convert::to_format(self, target).serialize(fps)
        }
    }

    /// Append a short attribution cue (e.g. a "synced with …" credit) that plays
    /// for `duration_ms`, starting `gap_ms` after the last existing cue ends, so
    /// it shows near the end without overlapping any dialogue. The text is
    /// encoded for this subtitle's format (a `Dialogue:` line for ASS, plain text
    /// otherwise). A no-op when there are no cues. Saturating arithmetic keeps the
    /// inserted times within `i64` bounds.
    pub fn append_credit(&mut self, text: &str, gap_ms: i64, duration_ms: i64) {
        let Some(last_end) = self.cues.iter().map(|c| c.end_ms).max() else {
            return;
        };
        let start = last_end.saturating_add(gap_ms);
        let end = start.saturating_add(duration_ms);
        // ASS payloads are whole `Dialogue:` lines (timing rewritten on
        // serialize); every other format stores the text directly.
        let payload = match self.format {
            Format::Ass => crate::ass::dialogue_line(text),
            _ => text.to_string(),
        };
        self.cues.push(Cue {
            start_ms: start,
            end_ms: end,
            payload,
        });
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
    fn format_from_extension_is_case_insensitive() {
        assert_eq!(Format::from_extension("srt"), Some(Format::SubRip));
        assert_eq!(Format::from_extension("VTT"), Some(Format::WebVtt));
        assert_eq!(Format::from_extension("sub"), Some(Format::MicroDvd));
        assert_eq!(Format::from_extension("Ass"), Some(Format::Ass));
        assert_eq!(Format::from_extension("ssa"), Some(Format::Ass));
        assert_eq!(Format::from_extension("txt"), None);
        assert_eq!(Format::from_extension(""), None);
    }

    #[test]
    fn parse_dispatches_by_format_and_wraps_errors() {
        let sub = Subtitle::parse(Format::SubRip, "00:00:01,000 --> 00:00:02,000\nHi\n", 25.0)
            .expect("valid SRT");
        assert_eq!(sub.format, Format::SubRip);
        assert_eq!(sub.cues.len(), 1);

        // A format-specific failure surfaces as the matching ParseError variant.
        let err = Subtitle::parse(Format::SubRip, "00:00:01,000 --> nope\nHi\n", 25.0)
            .expect_err("bad timestamp");
        assert!(matches!(err, ParseError::Srt(_)));
        // Parse then serialize round-trips through the shared dispatch.
        assert_eq!(
            Subtitle::parse(sub.format, &sub.serialize(25.0), 25.0).unwrap(),
            sub
        );
    }

    #[test]
    fn append_credit_adds_a_trailing_plain_text_cue() {
        let mut sub = Subtitle {
            format: Format::SubRip,
            header: String::new(),
            cues: vec![Cue::new(1_000, 2_000, "hi")],
        };
        sub.append_credit("Synced with subomatic.github.io", 1_000, 3_000);
        assert_eq!(sub.cues.len(), 2);
        let credit = sub.cues.last().unwrap();
        assert_eq!(credit.start_ms, 3_000); // last end (2000) + 1000 gap
        assert_eq!(credit.end_ms, 6_000); // + 3000 duration
        assert_eq!(credit.payload, "Synced with subomatic.github.io");
    }

    #[test]
    fn append_credit_on_ass_emits_a_dialogue_line_with_real_timing() {
        let ass = "[Events]\n\
                   Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
                   Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,hi\n";
        let mut sub = Subtitle::parse(Format::Ass, ass, 25.0).unwrap();
        sub.append_credit("synced", 1_000, 3_000);
        let out = sub.serialize(25.0);
        assert!(
            out.contains("Dialogue: 0,0:00:03.00,0:00:06.00,Default,,0,0,0,,synced"),
            "got {out}"
        );
    }

    #[test]
    fn append_credit_is_a_noop_without_cues() {
        let mut sub = Subtitle {
            format: Format::SubRip,
            header: String::new(),
            cues: vec![],
        };
        sub.append_credit("x", 1_000, 3_000);
        assert!(sub.cues.is_empty());
    }

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
