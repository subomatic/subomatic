// SPDX-License-Identifier: Apache-2.0
//! MicroDVD (`.sub`) parsing and serialization.
//!
//! MicroDVD is frame-based — `{startframe}{endframe}text` with `|` separating
//! text lines — so both parsing and serializing need the video frame rate to
//! convert frames↔milliseconds.
//!
//! A *leading* `{1}{1}fps` declaration line, if present, is consumed, preserved
//! verbatim in [`Subtitle::header`], and re-emitted; serialization uses that
//! declared rate, falling back to the supplied `fps` only when the file declares
//! none. Lines that don't match `{a}{b}text` are skipped.

use crate::cue::{Cue, Format, Subtitle};

/// Fallback frame rate when none is provided or declared (NTSC film, 23.976).
pub const DEFAULT_FPS: f64 = crate::NTSC_FILM_FPS;

/// Whether `fps` is a usable frame rate: positive and finite.
///
/// The single source of truth for frame-rate validity, shared by the CLI and
/// WASM front-ends (via [`invalid_fps_message`]) so they agree on what's valid.
pub fn is_valid_fps(fps: f64) -> bool {
    fps.is_finite() && fps > 0.0
}

/// The error message for a frame rate rejected by [`is_valid_fps`], so the CLI
/// and web front-ends report it identically.
pub fn invalid_fps_message(fps: f64) -> String {
    format!("fps must be positive and finite, got {fps}")
}

/// A usable frame rate: positive and finite, else [`DEFAULT_FPS`].
fn sane_fps(fps: f64) -> f64 {
    if is_valid_fps(fps) {
        fps
    } else {
        DEFAULT_FPS
    }
}

/// The frame rate declared by a `{1}{1}fps` header line, if any.
fn fps_from_header(header: &str) -> Option<f64> {
    let (start, end, text) = parse_line(header)?;
    if start != 1 || end != 1 {
        return None; // only a genuine {1}{1}fps record declares the rate
    }
    let rate: f64 = text.trim().parse().ok()?;
    is_valid_fps(rate).then_some(rate)
}

/// Parse MicroDVD text. `fps` converts frames to milliseconds; a leading
/// `{1}{1}fps` declaration overrides it and is preserved in the header.
pub fn parse(input: &str, fps: f64) -> Subtitle {
    let normalized = crate::text::normalize(input);

    let mut rate = sane_fps(fps);
    let mut header = String::new();
    let mut cues = Vec::new();

    for line in normalized.lines() {
        let Some((start_frame, end_frame, text)) = parse_line(line) else {
            continue;
        };
        // Only a LEADING {1}{1}<number> line (before any cue) is a rate
        // declaration; the same shape later is a real one-frame cue.
        if cues.is_empty() && header.is_empty() && start_frame == 1 && end_frame == 1 {
            if let Ok(declared) = text.trim().parse::<f64>() {
                if is_valid_fps(declared) {
                    rate = declared;
                    header = line.to_string(); // preserve the declaration verbatim
                    continue;
                }
            }
        }
        cues.push(Cue::new(
            frame_to_ms(start_frame, rate),
            frame_to_ms(end_frame, rate),
            text.replace('|', "\n"),
        ));
    }

    Subtitle {
        format: Format::MicroDvd,
        header,
        cues,
    }
}

/// Serialize a [`Subtitle`] to MicroDVD. A frame rate declared in the header is
/// authoritative; otherwise `fps` is used.
pub fn serialize(sub: &Subtitle, fps: f64) -> String {
    let rate = fps_from_header(&sub.header).unwrap_or_else(|| sane_fps(fps));
    let mut out = String::new();
    if !sub.header.is_empty() {
        out.push_str(&sub.header);
        out.push('\n');
    }
    for cue in &sub.cues {
        out.push_str(&format!(
            "{{{}}}{{{}}}{}\n",
            ms_to_frame(cue.start_ms, rate),
            ms_to_frame(cue.end_ms, rate),
            cue.payload.replace('\n', "|")
        ));
    }
    out
}

/// Parse one `{start}{end}text` line into `(start_frame, end_frame, text)`.
fn parse_line(line: &str) -> Option<(i64, i64, String)> {
    let rest = line.trim().strip_prefix('{')?;
    let (start, rest) = rest.split_once('}')?;
    let rest = rest.strip_prefix('{')?;
    let (end, text) = rest.split_once('}')?;
    let start: i64 = start.trim().parse().ok()?;
    let end: i64 = end.trim().parse().ok()?;
    if start < 0 || end < 0 {
        return None;
    }
    Some((start, end, text.to_string()))
}

fn frame_to_ms(frame: i64, fps: f64) -> i64 {
    (frame as f64 * 1000.0 / fps).round() as i64
}

fn ms_to_frame(ms: i64, fps: f64) -> i64 {
    (ms.max(0) as f64 * fps / 1000.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frames_to_ms_and_round_trips() {
        let fps = 25.0;
        let input = "{25}{50}Hello\n{75}{100}World|second line\n";
        let sub = parse(input, fps);
        assert_eq!(sub.format, Format::MicroDvd);
        assert_eq!(sub.cues.len(), 2);
        // 25 frames / 25 fps = 1.0 s.
        assert_eq!(sub.cues[0].start_ms, 1_000);
        assert_eq!(sub.cues[0].end_ms, 2_000);
        assert_eq!(sub.cues[0].payload, "Hello");
        assert_eq!(sub.cues[1].payload, "World\nsecond line");

        let out = serialize(&sub, fps);
        assert!(out.contains("{25}{50}Hello"));
        assert!(out.contains("{75}{100}World|second line"));
    }

    #[test]
    fn consumes_leading_fps_declaration_line() {
        let sub = parse("{1}{1}25.000\n{25}{50}Hi\n", DEFAULT_FPS);
        assert_eq!(sub.cues.len(), 1);
        assert_eq!(sub.cues[0].start_ms, 1_000); // 25 frames @ 25 fps
    }

    #[test]
    fn round_trips_declared_fps() {
        // The declared 25 fps must drive serialization, not the (different) fallback.
        let sub = parse("{1}{1}25.000\n{25}{50}Hi\n", DEFAULT_FPS);
        assert_eq!(sub.cues[0].start_ms, 1_000);
        let out = serialize(&sub, DEFAULT_FPS);
        assert!(out.contains("{1}{1}25.000"), "declaration lost: {out}");
        assert!(
            out.contains("{25}{50}Hi"),
            "wrong frames (must use declared fps): {out}"
        );
    }

    #[test]
    fn frame_one_record_after_a_cue_is_not_a_declaration() {
        let sub = parse("{25}{50}Hi\n{1}{1}123\n", 25.0);
        assert_eq!(sub.cues.len(), 2);
        assert_eq!(sub.cues[1].payload, "123");
    }

    #[test]
    fn non_finite_fps_falls_back_to_default() {
        let sub = parse("{24}{48}Hi\n", f64::INFINITY);
        assert!(sub.cues[0].start_ms > 0);
    }

    #[test]
    fn header_fps_only_from_a_one_one_record() {
        assert_eq!(fps_from_header("{1}{1}25"), Some(25.0));
        assert_eq!(fps_from_header("{5}{9}30"), None);
        assert_eq!(fps_from_header("garbage"), None);
    }
}
