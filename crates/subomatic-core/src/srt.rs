// SPDX-License-Identifier: Apache-2.0
//! SubRip (`.srt`) parsing and serialization.
//!
//! Tolerant on input (optional indices, `,` or `.` decimal separators, CRLF or
//! LF line endings, a leading UTF-8 BOM, whitespace-only separator lines, and
//! trailing cue-position coordinates) and canonical on output.

use crate::cue::{Cue, Format, Subtitle};
use std::fmt;

/// An error encountered while parsing SubRip text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SrtError {
    /// A block had no `-->` timing line. Carries the 1-based block number.
    MissingTiming(usize),
    /// A timestamp could not be parsed (malformed, negative, or out of range).
    /// Carries the offending text.
    BadTimestamp(String),
}

impl fmt::Display for SrtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SrtError::MissingTiming(n) => write!(f, "subtitle block {n} has no timing line"),
            SrtError::BadTimestamp(s) => write!(f, "bad SRT timestamp: {s:?}"),
        }
    }
}

impl std::error::Error for SrtError {}

/// Parse SubRip text into a [`Subtitle`].
pub fn parse(input: &str) -> Result<Subtitle, SrtError> {
    // Strip a leading UTF-8 BOM, then normalize line endings.
    let normalized = crate::text::normalize(input);

    let mut cues = Vec::new();
    let mut block: Vec<&str> = Vec::new();

    // Cues are separated by blank lines; treat any whitespace-only line as a
    // separator (some files pad the blank line with spaces or tabs). The
    // trailing empty string flushes the final block.
    for line in normalized.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !block.is_empty() {
                let number = cues.len() + 1;
                cues.push(parse_block(&block, number)?);
                block.clear();
            }
        } else {
            block.push(line);
        }
    }

    Ok(Subtitle {
        format: Format::SubRip,
        header: String::new(),
        cues,
    })
}

/// Parse one non-empty block of lines into a [`Cue`]. `number` is the 1-based
/// cue index, used only for error messages.
fn parse_block(lines: &[&str], number: usize) -> Result<Cue, SrtError> {
    // The first line is either the optional numeric index or the timing line;
    // the timing line is the one containing "-->".
    let (timing, payload_start) = if lines[0].contains("-->") {
        (lines[0], 1)
    } else if lines.len() >= 2 && lines[1].contains("-->") {
        (lines[1], 2)
    } else {
        return Err(SrtError::MissingTiming(number));
    };

    let (start_ms, end_ms) = parse_timing(timing)?;
    let payload = lines[payload_start..].join("\n");
    Ok(Cue {
        start_ms,
        end_ms,
        payload,
    })
}

/// Serialize a [`Subtitle`] back to canonical SubRip text.
pub fn serialize(sub: &Subtitle) -> String {
    let mut out = String::new();
    for (index, cue) in sub.cues.iter().enumerate() {
        out.push_str(&format!("{}\n", index + 1));
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

fn parse_timing(line: &str) -> Result<(i64, i64), SrtError> {
    let (left, right) = line
        .split_once("-->")
        .ok_or_else(|| SrtError::BadTimestamp(line.to_string()))?;
    let start = parse_timestamp(left.trim())?;
    // The end timestamp may be followed by position coordinates; ignore them.
    let end_token = right.split_whitespace().next().unwrap_or_default();
    let end = parse_timestamp(end_token)?;
    Ok((start, end))
}

/// Parse `HH:MM:SS,mmm` (or `.mmm`) into milliseconds, using checked arithmetic
/// so malformed huge fields can't overflow.
fn parse_timestamp(s: &str) -> Result<i64, SrtError> {
    let (hms, millis) = s
        .split_once(',')
        .or_else(|| s.split_once('.'))
        .ok_or_else(|| SrtError::BadTimestamp(s.to_string()))?;

    let mut parts = hms.split(':');
    let hours = parse_field(parts.next(), s)?;
    let minutes = parse_field(parts.next(), s)?;
    let seconds = parse_field(parts.next(), s)?;
    if parts.next().is_some() {
        return Err(SrtError::BadTimestamp(s.to_string()));
    }
    let ms = parse_field(Some(millis), s)?;

    crate::text::hms_to_ms(hours, minutes, seconds, ms)
        .ok_or_else(|| SrtError::BadTimestamp(s.to_string()))
}

/// Parse a single non-negative integer timestamp field.
fn parse_field(field: Option<&str>, whole: &str) -> Result<i64, SrtError> {
    let value: i64 = field
        .ok_or_else(|| SrtError::BadTimestamp(whole.to_string()))?
        .trim()
        .parse()
        .map_err(|_| SrtError::BadTimestamp(whole.to_string()))?;
    if value < 0 {
        return Err(SrtError::BadTimestamp(whole.to_string()));
    }
    Ok(value)
}

fn format_timestamp(ms: i64) -> String {
    let (hours, minutes, seconds, millis) = crate::text::decompose_ms(ms);
    format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cues_and_round_trips() {
        let input = "1\n00:00:01,000 --> 00:00:02,000\nHello\n\n\
                     2\n00:00:05,500 --> 00:00:06,250\nWorld\nsecond line\n";

        let sub = parse(input).unwrap();
        assert_eq!(sub.cues.len(), 2);
        assert_eq!(sub.cues[0].start_ms, 1000);
        assert_eq!(sub.cues[0].end_ms, 2000);
        assert_eq!(sub.cues[0].payload, "Hello");
        assert_eq!(sub.cues[1].start_ms, 5500);
        assert_eq!(sub.cues[1].end_ms, 6250);
        assert_eq!(sub.cues[1].payload, "World\nsecond line");

        // Parsing our own output must reproduce the same cues.
        let reparsed = parse(&serialize(&sub)).unwrap();
        assert_eq!(reparsed.cues, sub.cues);
    }

    #[test]
    fn accepts_index_less_and_dot_decimal_input() {
        let sub = parse("00:00:01.000 --> 00:00:02.000\nNo index here\n").unwrap();
        assert_eq!(sub.cues.len(), 1);
        assert_eq!(sub.cues[0].start_ms, 1000);
        assert_eq!(sub.cues[0].end_ms, 2000);
        assert_eq!(sub.cues[0].payload, "No index here");
    }

    #[test]
    fn treats_whitespace_only_lines_as_separators() {
        // The "blank" line between the two cues contains spaces.
        let input = "00:00:01,000 --> 00:00:02,000\nA\n   \n00:00:03,000 --> 00:00:04,000\nB\n";
        let sub = parse(input).unwrap();
        assert_eq!(sub.cues.len(), 2);
        assert_eq!(sub.cues[0].payload, "A");
        assert_eq!(sub.cues[1].payload, "B");
    }

    #[test]
    fn strips_leading_bom() {
        let sub = parse("\u{feff}00:00:01,000 --> 00:00:02,000\nHi\n").unwrap();
        assert_eq!(sub.cues.len(), 1);
        assert_eq!(sub.cues[0].start_ms, 1000);
    }

    #[test]
    fn rejects_overflowing_timestamp() {
        // An hours field this large overflows i64 milliseconds.
        let input = "9999999999999:00:00,000 --> 9999999999999:00:01,000\nX\n";
        assert!(parse(input).is_err());
    }

    #[test]
    fn rejects_negative_timestamp_field() {
        assert!(parse("00:-5:00,000 --> 00:00:01,000\nX\n").is_err());
    }
}
