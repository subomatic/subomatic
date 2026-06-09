// SPDX-License-Identifier: Apache-2.0
//! Advanced SubStation Alpha (`.ass` / `.ssa`) parsing and serialization.
//!
//! Timing-only and high-fidelity: the preamble (`[Script Info]`, styles, and the
//! `[Events]` `Format:` line) is preserved verbatim in [`Subtitle::header`], and
//! each `Dialogue:`/`Comment:` line is kept verbatim in its cue payload. Only the
//! `Start`/`End` fields (located via the `[Events]` `Format:` line) are rewritten
//! on serialize, so styles, karaoke tags, and every other field round-trip
//! untouched.
//!
//! First-cut limitations: content appearing *after* the `[Events]` section (e.g.
//! trailing `[Fonts]`/`[Graphics]`) is not preserved, and minor whitespace /
//! line-ending formatting may be normalized on output.

use crate::cue::{Cue, Format, Subtitle};
use std::fmt;

/// An error encountered while parsing ASS/SSA text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AssError {
    /// No usable `[Events]` `Format:` line (so Start/End columns are unknown).
    MissingEventsFormat,
    /// A `Dialogue:`/`Comment:` line had unparseable Start/End timing.
    BadTiming(String),
}

impl fmt::Display for AssError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssError::MissingEventsFormat => write!(f, "ASS file has no [Events] Format: line"),
            AssError::BadTiming(s) => write!(f, "bad ASS dialogue timing: {s:?}"),
        }
    }
}

impl std::error::Error for AssError {}

/// Column layout of the `[Events]` `Format:` line.
#[derive(Clone, Copy)]
struct Columns {
    start: usize,
    end: usize,
    count: usize,
}

/// Parse the `Start`/`End` column indices and field count from a `Format:` line.
fn parse_columns(line: &str) -> Option<Columns> {
    let trimmed = line.trim_start();
    let rest = trimmed
        .get(..7)
        .filter(|prefix| prefix.eq_ignore_ascii_case("Format:"))
        .map(|_| &trimmed[7..])?;
    let names: Vec<String> = rest
        .split(',')
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    let start = names.iter().position(|n| n == "start")?;
    let end = names.iter().position(|n| n == "end")?;
    Some(Columns {
        start,
        end,
        count: names.len(),
    })
}

/// Whether this line is a `Dialogue:` or `Comment:` event line.
fn is_event(line: &str) -> bool {
    let t = line.trim_start();
    // `get` is char-boundary-safe, so non-ASCII lines can't panic the slice.
    let starts_with_ci = |p: &str| t.get(..p.len()).is_some_and(|s| s.eq_ignore_ascii_case(p));
    starts_with_ci("Dialogue:") || starts_with_ci("Comment:")
}

/// Split an event line into its leading `Dialogue:`/`Comment:` token and its
/// `count` fields (the last field keeps any embedded commas).
fn split_event(line: &str, count: usize) -> Option<(&str, Vec<&str>)> {
    let colon = line.find(':')?;
    let (prefix, rest) = line.split_at(colon + 1);
    Some((prefix, rest.splitn(count, ',').collect()))
}

/// Parse ASS time `H:MM:SS.cc` (centiseconds) into milliseconds.
fn parse_time(s: &str) -> Option<i64> {
    let (hms, centis) = s.trim().split_once('.')?;
    let mut parts = hms.split(':');
    let hours: i64 = parts.next()?.trim().parse().ok()?;
    let minutes: i64 = parts.next()?.trim().parse().ok()?;
    let seconds: i64 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let centis = centis.trim();
    if centis.len() != 2 || !centis.bytes().all(|b| b.is_ascii_digit()) {
        return None; // ASS centiseconds are always exactly two digits
    }
    let centis: i64 = centis.parse().ok()?;
    if hours < 0 || minutes < 0 || seconds < 0 {
        return None;
    }
    // ASS sub-seconds are centiseconds; convert to ms for the shared accumulator.
    crate::text::hms_to_ms(hours, minutes, seconds, centis.checked_mul(10)?)
}

/// Format milliseconds as ASS time `H:MM:SS.cc`.
fn format_time(ms: i64) -> String {
    let (hours, minutes, seconds, millis) = crate::text::decompose_ms(ms);
    let centis = millis / 10;
    format!("{hours}:{minutes:02}:{seconds:02}.{centis:02}")
}

/// Parse ASS/SSA text into a [`Subtitle`].
pub fn parse(input: &str) -> Result<Subtitle, AssError> {
    let normalized = crate::text::normalize(input);

    let mut columns: Option<Columns> = None;
    let mut header_lines: Vec<&str> = Vec::new();
    let mut cues = Vec::new();
    let mut in_events_body = false;

    for line in normalized.lines() {
        // The [Events] Format: line is the last Format: line before the events.
        if !in_events_body {
            if let Some(cols) = parse_columns(line) {
                columns = Some(cols);
            }
        }

        if is_event(line) {
            in_events_body = true;
            let cols = columns.ok_or(AssError::MissingEventsFormat)?;
            let (_, fields) = split_event(line, cols.count)
                .ok_or_else(|| AssError::BadTiming(line.to_string()))?;
            let start = fields
                .get(cols.start)
                .and_then(|f| parse_time(f))
                .ok_or_else(|| AssError::BadTiming(line.to_string()))?;
            let end = fields
                .get(cols.end)
                .and_then(|f| parse_time(f))
                .ok_or_else(|| AssError::BadTiming(line.to_string()))?;
            cues.push(Cue::new(start, end, line.to_string()));
        } else if !in_events_body {
            header_lines.push(line);
        }
        // Non-event lines after the events begin are rare; left out (kept simple).
    }

    Ok(Subtitle {
        format: Format::Ass,
        header: header_lines.join("\n"),
        cues,
    })
}

/// Serialize a [`Subtitle`] back to ASS/SSA, rewriting only Start/End.
pub fn serialize(sub: &Subtitle) -> String {
    // Recover the column layout from the header's last Format: line.
    let columns = sub.header.lines().filter_map(parse_columns).next_back();

    let mut out = String::new();
    out.push_str(&sub.header);
    out.push('\n');
    for cue in &sub.cues {
        out.push_str(&rewrite_event(
            &cue.payload,
            columns,
            cue.start_ms,
            cue.end_ms,
        ));
        out.push('\n');
    }
    out
}

/// Rewrite the Start/End fields of one event line; if the layout is unknown or
/// the line is malformed, emit it unchanged (parsed cues are well-formed).
fn rewrite_event(line: &str, columns: Option<Columns>, start_ms: i64, end_ms: i64) -> String {
    let Some(cols) = columns else {
        return line.to_string();
    };
    let Some((prefix, fields)) = split_event(line, cols.count) else {
        return line.to_string();
    };
    if cols.start >= fields.len() || cols.end >= fields.len() {
        return line.to_string();
    }
    let mut fields: Vec<String> = fields.iter().map(|&f| f.to_string()).collect();
    fields[cols.start] = format_time(start_ms);
    fields[cols.end] = format_time(end_ms);
    format!("{prefix}{}", fields.join(","))
}

/// A minimal but valid ASS preamble for subtitles *converted* from another
/// format (which carry no ASS styling of their own). Holds a single `Default`
/// style and an `[Events]` `Format:` line whose columns match [`dialogue_line`]
/// (Start at index 1, End at 2, Text last). No trailing newline — [`serialize`]
/// adds the separator before the events.
pub(crate) const DEFAULT_HEADER: &str = "[Script Info]
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";

/// Build a `Dialogue:` line for `text` under [`DEFAULT_HEADER`]'s layout, with
/// placeholder timing ([`serialize`] rewrites Start/End from the cue). Newlines
/// become ASS hard breaks (`\N`); the text is the last field, so its commas are
/// harmless.
pub(crate) fn dialogue_line(text: &str) -> String {
    format!(
        "Dialogue: 0,0:00:00.00,0:00:00.00,Default,,0,0,0,,{}",
        text.replace('\n', "\\N")
    )
}

/// The `(index, field_count)` of the `Text` column, from the last `Format:` line
/// that declares one (the `[Events]` line — `[V4+ Styles]` has no `Text`).
fn text_column(header: &str) -> Option<(usize, usize)> {
    header.lines().rev().find_map(|line| {
        let trimmed = line.trim_start();
        let rest = trimmed
            .get(..7)
            .filter(|p| p.eq_ignore_ascii_case("Format:"))
            .map(|_| &trimmed[7..])?;
        let names: Vec<String> = rest
            .split(',')
            .map(|n| n.trim().to_ascii_lowercase())
            .collect();
        let idx = names.iter().position(|n| n == "text")?;
        Some((idx, names.len()))
    })
}

/// Extract the plain display text of one event `line` (locating the Text field
/// via `header`'s columns), dropping `{\...}` override blocks and turning ASS
/// escapes into plain text (`\N`/`\n` → newline, `\h` → space). Used when
/// converting an ASS subtitle into a format that can't carry its styling.
pub(crate) fn event_plain_text(header: &str, line: &str) -> String {
    let field = text_column(header)
        .and_then(|(idx, count)| Some(split_event(line, count)?.1.get(idx)?.to_string()));
    clean_text(field.as_deref().unwrap_or(line))
}

/// Strip ASS override blocks and decode the common escapes to plain text.
fn clean_text(text: &str) -> String {
    let mut out = String::new();
    let mut depth: u32 = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            '\\' => match chars.peek() {
                Some('N') | Some('n') => {
                    out.push('\n');
                    chars.next();
                }
                Some('h') => {
                    out.push(' ');
                    chars.next();
                }
                _ => out.push('\\'),
            },
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[Script Info]
Title: Demo
ScriptType: v4.00+

[V4+ Styles]
Format: Name, Fontname, Fontsize
Style: Default,Arial,20

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hello, world
Comment: 0,0:00:03.00,0:00:04.50,Default,,0,0,0,,note
";

    #[test]
    fn parses_dialogue_timing_and_preserves_text_with_commas() {
        let sub = parse(SAMPLE).unwrap();
        assert_eq!(sub.format, Format::Ass);
        assert_eq!(sub.cues.len(), 2);
        assert_eq!(sub.cues[0].start_ms, 1_000);
        assert_eq!(sub.cues[0].end_ms, 2_000);
        // The Text field ("Hello, world") contains a comma and must survive.
        assert!(sub.cues[0].payload.ends_with("Hello, world"));
        assert_eq!(sub.cues[1].start_ms, 3_000);
        assert_eq!(sub.cues[1].end_ms, 4_500);
    }

    #[test]
    fn round_trip_preserves_header_and_fields() {
        let sub = parse(SAMPLE).unwrap();
        let out = serialize(&sub);
        assert!(out.contains("[Script Info]"));
        assert!(out.contains("Style: Default,Arial,20"));
        assert!(out.contains("Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hello, world"));
        let reparsed = parse(&out).unwrap();
        assert_eq!(reparsed.cues, sub.cues);
    }

    #[test]
    fn serialize_after_shift_rewrites_only_timing() {
        let mut sub = parse(SAMPLE).unwrap();
        sub.shift_all(500);
        let out = serialize(&sub);
        // First cue moves 1.0->1.5 s and 2.0->2.5 s; everything else stays.
        assert!(out.contains("Dialogue: 0,0:00:01.50,0:00:02.50,Default,,0,0,0,,Hello, world"));
    }

    #[test]
    fn missing_events_format_errors() {
        let bad = "[Events]\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hi\n";
        assert_eq!(parse(bad).unwrap_err(), AssError::MissingEventsFormat);
    }

    #[test]
    fn non_ascii_lines_do_not_panic() {
        // A non-event line with multibyte chars must not panic the prefix check.
        let input = "[Script Info]\nTitle: éàü\n\n[Events]\n\
                     Format: Layer, Start, End, Text\nDialogue: 0,0:00:01.00,0:00:02.00,héllo\n";
        let sub = parse(input).unwrap();
        assert_eq!(sub.cues.len(), 1);
        assert_eq!(sub.cues[0].start_ms, 1_000);
    }

    #[test]
    fn rejects_non_two_digit_centiseconds() {
        let bad = "[Events]\nFormat: Layer, Start, End, Text\n\
                   Dialogue: 0,0:00:01.234,0:00:02.00,x\n";
        assert!(matches!(parse(bad).unwrap_err(), AssError::BadTiming(_)));
    }
}
