// SPDX-License-Identifier: Apache-2.0
//! WebVTT (`.vtt`) parsing and serialization.
//!
//! The format preamble (the `WEBVTT` line and any `STYLE`/`NOTE`/`REGION`
//! blocks) is preserved in [`Subtitle::header`], and each cue's text in its
//! payload. Cues are separated by blank lines (any whitespace-only line counts).
//!
//! First-cut limitations (documented follow-ups): per-cue identifiers and cue
//! settings (position/line/region/…) are dropped and timing lines re-emitted
//! canonically; `NOTE`/`STYLE`/`REGION` blocks appearing *between* cues are
//! folded into the preamble (so they re-emit before all cues); out-of-range
//! timestamp fields are tolerated and normalized rather than rejected.

use crate::cue::{Cue, Format, Subtitle};
use std::fmt;

/// An error encountered while parsing WebVTT text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VttError {
    /// The input did not start with the required `WEBVTT` signature.
    MissingHeader,
    /// A timestamp could not be parsed.
    BadTimestamp(String),
}

impl fmt::Display for VttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VttError::MissingHeader => write!(f, "not a WebVTT file (missing WEBVTT signature)"),
            VttError::BadTimestamp(s) => write!(f, "bad WebVTT timestamp: {s:?}"),
        }
    }
}

impl std::error::Error for VttError {}

/// Parse WebVTT text into a [`Subtitle`].
pub fn parse(input: &str) -> Result<Subtitle, VttError> {
    let normalized = crate::text::normalize(input);

    if !normalized
        .lines()
        .next()
        .unwrap_or_default()
        .starts_with("WEBVTT")
    {
        return Err(VttError::MissingHeader);
    }

    let mut header_blocks: Vec<String> = Vec::new();
    let mut cues = Vec::new();
    let mut block: Vec<&str> = Vec::new();

    // Group lines into blocks separated by blank (whitespace-only) lines; the
    // trailing empty string flushes the final block.
    for line in normalized.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !block.is_empty() {
                push_block(&block, &mut header_blocks, &mut cues)?;
                block.clear();
            }
        } else {
            block.push(line);
        }
    }

    Ok(Subtitle {
        format: Format::WebVtt,
        header: header_blocks.join("\n\n"),
        cues,
    })
}

/// Classify one block: a cue block contains a `-->` line; anything else is
/// preamble (the `WEBVTT` line or a `STYLE`/`NOTE`/`REGION` block).
fn push_block(
    lines: &[&str],
    header_blocks: &mut Vec<String>,
    cues: &mut Vec<Cue>,
) -> Result<(), VttError> {
    match lines.iter().position(|l| l.contains("-->")) {
        Some(timing_idx) => {
            // Lines before the timing line are the (dropped) identifier; the rest
            // after it is the cue text.
            let (start_ms, end_ms) = parse_timing(lines[timing_idx])?;
            let payload = lines[timing_idx + 1..].join("\n");
            cues.push(Cue::new(start_ms, end_ms, payload));
        }
        None => header_blocks.push(lines.join("\n")),
    }
    Ok(())
}

/// Serialize a [`Subtitle`] back to canonical WebVTT text.
pub fn serialize(sub: &Subtitle) -> String {
    let mut out = String::new();
    let header = sub.header.trim_end();
    out.push_str(if header.is_empty() { "WEBVTT" } else { header });
    out.push_str("\n\n");
    for cue in &sub.cues {
        out.push_str(&format!(
            "{} --> {}\n",
            format_timestamp(cue.start_ms),
            format_timestamp(cue.end_ms)
        ));
        out.push_str(&cue.payload);
        out.push_str("\n\n");
    }
    out
}

fn parse_timing(line: &str) -> Result<(i64, i64), VttError> {
    let (left, right) = line
        .split_once("-->")
        .ok_or_else(|| VttError::BadTimestamp(line.to_string()))?;
    let start = parse_timestamp(left.trim())?;
    // The end timestamp may be followed by cue settings; ignore them.
    let end_token = right.split_whitespace().next().unwrap_or_default();
    let end = parse_timestamp(end_token)?;
    Ok((start, end))
}

/// Parse `HH:MM:SS.mmm` or `MM:SS.mmm` into milliseconds (checked, non-negative).
fn parse_timestamp(s: &str) -> Result<i64, VttError> {
    let (hms, millis) = s
        .split_once('.')
        .ok_or_else(|| VttError::BadTimestamp(s.to_string()))?;
    let parts: Vec<i64> = hms
        .split(':')
        .map(|p| p.trim().parse::<i64>())
        .collect::<Result<_, _>>()
        .map_err(|_| VttError::BadTimestamp(s.to_string()))?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [m, sec] => (0, *m, *sec),
        [h, m, sec] => (*h, *m, *sec),
        _ => return Err(VttError::BadTimestamp(s.to_string())),
    };
    let ms: i64 = millis
        .trim()
        .parse()
        .map_err(|_| VttError::BadTimestamp(s.to_string()))?;
    if hours < 0 || minutes < 0 || seconds < 0 || ms < 0 {
        return Err(VttError::BadTimestamp(s.to_string()));
    }
    crate::text::hms_to_ms(hours, minutes, seconds, ms)
        .ok_or_else(|| VttError::BadTimestamp(s.to_string()))
}

fn format_timestamp(ms: i64) -> String {
    let (hours, minutes, seconds, millis) = crate::text::decompose_ms(ms);
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cues_and_round_trips() {
        let input = "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHello\n\n\
                     00:00:05.500 --> 00:00:06.250 position:50%\nWorld\nline2\n";
        let sub = parse(input).unwrap();
        assert_eq!(sub.format, Format::WebVtt);
        assert_eq!(sub.cues.len(), 2);
        assert_eq!(sub.cues[0].start_ms, 1_000);
        assert_eq!(sub.cues[0].end_ms, 2_000);
        assert_eq!(sub.cues[0].payload, "Hello");
        assert_eq!(sub.cues[1].start_ms, 5_500);
        assert_eq!(sub.cues[1].payload, "World\nline2");

        let reparsed = parse(&serialize(&sub)).unwrap();
        assert_eq!(reparsed.cues, sub.cues);
    }

    #[test]
    fn accepts_short_timestamps_and_requires_signature() {
        let sub = parse("WEBVTT\n\n01:02.500 --> 01:03.000\nHi\n").unwrap();
        assert_eq!(sub.cues[0].start_ms, 62_500);
        assert!(parse("nope\n\n00:00:01.000 --> 00:00:02.000\nHi").is_err());
    }

    #[test]
    fn preserves_style_block_in_header() {
        let input =
            "WEBVTT\n\nSTYLE\n::cue { color: yellow }\n\n00:00:01.000 --> 00:00:02.000\nHi\n";
        let sub = parse(input).unwrap();
        assert!(sub.header.contains("STYLE"));
        assert!(serialize(&sub).contains("::cue { color: yellow }"));
    }

    #[test]
    fn treats_whitespace_only_lines_as_separators() {
        // The "blank" lines between blocks contain spaces.
        let input =
            "WEBVTT\n \n00:00:01.000 --> 00:00:02.000\nA\n  \n00:00:03.000 --> 00:00:04.000\nB\n";
        let sub = parse(input).unwrap();
        assert_eq!(sub.cues.len(), 2);
        assert_eq!(sub.cues[0].payload, "A");
        assert_eq!(sub.cues[1].payload, "B");
    }
}
