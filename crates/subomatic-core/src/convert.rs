// SPDX-License-Identifier: Apache-2.0
//! Cross-format conversion of a parsed [`Subtitle`].
//!
//! Synchronization is timing-only and normally re-emits a subtitle in its
//! source format. This module lets a caller request a *different* output format.
//!
//! Fidelity is "smart": SubRip and WebVTT share the same HTML-ish inline markup
//! (`<i>`, `<b>`, …), so converting between them keeps each cue's payload
//! verbatim. Crossing into or out of MicroDVD or ASS — whose markup is
//! incompatible — flattens cues to plain text (styling is dropped) and rebuilds
//! a minimal header for the target.

use crate::ass;
use crate::cue::{Cue, Format, Subtitle};

/// Convert `sub` to `target`, translating each cue's payload and the header.
/// A no-op clone when `target` already matches (the full-fidelity path).
pub(crate) fn to_format(sub: &Subtitle, target: Format) -> Subtitle {
    if target == sub.format {
        return sub.clone();
    }
    // SubRip <-> WebVTT can carry inline markup across unchanged; every other
    // crossing must flatten to plain text.
    let keep_markup = is_html_ish(sub.format) && is_html_ish(target);
    let cues = sub
        .cues
        .iter()
        .map(|cue| Cue {
            start_ms: cue.start_ms,
            end_ms: cue.end_ms,
            payload: if keep_markup {
                cue.payload.clone()
            } else {
                encode(target, &plain_text(sub.format, &sub.header, &cue.payload))
            },
        })
        .collect();
    Subtitle {
        format: target,
        header: header_for(target),
        cues,
    }
}

/// Whether a format uses SRT/VTT-style inline HTML markup (so the two are
/// markup-compatible with each other).
fn is_html_ish(format: Format) -> bool {
    matches!(format, Format::SubRip | Format::WebVtt)
}

/// The plain display text of a cue payload in its source format, with `\n` line
/// breaks and all format-specific markup removed.
fn plain_text(format: Format, header: &str, payload: &str) -> String {
    match format {
        Format::SubRip | Format::WebVtt => strip_html(payload),
        Format::MicroDvd => strip_braces(payload),
        Format::Ass => ass::event_plain_text(header, payload),
    }
}

/// Encode already-plain `text` into a cue payload for `target`.
fn encode(target: Format, text: &str) -> String {
    match target {
        // SubRip and MicroDVD take the text directly (MicroDVD's serializer
        // converts the newlines to `|`).
        Format::SubRip | Format::MicroDvd => text.to_string(),
        // WebVTT treats `&` and `<` specially; escape them so flattened text
        // can't accidentally look like markup.
        Format::WebVtt => escape_vtt(text),
        Format::Ass => ass::dialogue_line(text),
    }
}

/// The header a converted subtitle needs for `target`: a synthesized ASS
/// preamble, or none (SubRip/MicroDVD carry no header; WebVTT's serializer emits
/// the `WEBVTT` signature from an empty header).
fn header_for(target: Format) -> String {
    match target {
        Format::Ass => ass::DEFAULT_HEADER.to_string(),
        _ => String::new(),
    }
}

/// Strip `<...>` tags and decode the handful of HTML entities SRT/VTT use.
fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            _ => out.push(c),
        }
    }
    // Decode `&amp;` last so an encoded entity like `&amp;lt;` survives as text.
    out.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Remove `{...}` control codes (MicroDVD style tags), keeping the text.
fn strip_braces(s: &str) -> String {
    let mut out = String::new();
    let mut depth: u32 = 0;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Escape the WebVTT-special characters in plain text (`&` first).
fn escape_vtt(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Subtitle;

    fn parse(format: Format, text: &str) -> Subtitle {
        Subtitle::parse(format, text, 25.0).unwrap()
    }

    #[test]
    fn srt_to_vtt_keeps_inline_markup() {
        let sub = parse(
            Format::SubRip,
            "1\n00:00:01,000 --> 00:00:02,000\n<i>Hi</i>\n",
        );
        let out = to_format(&sub, Format::WebVtt);
        assert_eq!(out.format, Format::WebVtt);
        assert_eq!(out.cues[0].payload, "<i>Hi</i>"); // markup preserved
                                                      // Re-serializes to valid VTT with the same timing.
        let text = out.serialize(25.0);
        assert!(text.starts_with("WEBVTT"));
        assert!(text.contains("<i>Hi</i>"));
    }

    #[test]
    fn ass_to_srt_flattens_to_plain_text() {
        let ass = "[Events]\n\
                   Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
                   Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,{\\i1}Hello{\\i0}\\Nworld\n";
        let sub = parse(Format::Ass, ass);
        let out = to_format(&sub, Format::SubRip);
        assert_eq!(out.format, Format::SubRip);
        // Override tags gone; the hard break became a newline.
        assert_eq!(out.cues[0].payload, "Hello\nworld");
        assert_eq!(out.cues[0].start_ms, 1_000);
    }

    #[test]
    fn srt_to_ass_synthesizes_a_valid_dialogue_that_reparses() {
        let sub = parse(
            Format::SubRip,
            "1\n00:00:01,000 --> 00:00:02,000\nLine one\nLine two\n",
        );
        let out = to_format(&sub, Format::Ass);
        let text = out.serialize(25.0);
        assert!(text.contains("[Events]"));
        // Real timing was written, and the newline became an ASS hard break.
        assert!(
            text.contains("Dialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Line one\\NLine two")
        );
        // The synthesized ASS round-trips back through the parser.
        let reparsed = parse(Format::Ass, &text);
        assert_eq!(reparsed.cues.len(), 1);
        assert_eq!(reparsed.cues[0].start_ms, 1_000);
        assert_eq!(reparsed.cues[0].end_ms, 2_000);
    }

    #[test]
    fn microdvd_to_srt_strips_style_codes() {
        let sub = parse(Format::MicroDvd, "{25}{50}{y:i}Hello|world\n");
        let out = to_format(&sub, Format::SubRip);
        assert_eq!(out.cues[0].payload, "Hello\nworld");
    }

    #[test]
    fn flatten_into_vtt_escapes_specials() {
        let sub = parse(Format::MicroDvd, "{25}{50}5 < 10 & up\n");
        let out = to_format(&sub, Format::WebVtt);
        assert_eq!(out.cues[0].payload, "5 &lt; 10 &amp; up");
    }

    #[test]
    fn same_format_is_an_identity_clone() {
        let sub = parse(
            Format::SubRip,
            "1\n00:00:01,000 --> 00:00:02,000\n<i>Hi</i>\n",
        );
        assert_eq!(to_format(&sub, Format::SubRip), sub);
    }
}
