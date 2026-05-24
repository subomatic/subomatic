// SPDX-License-Identifier: Apache-2.0
//! MicroDVD (`.sub`) parsing and serialization.
//!
//! MicroDVD is frame-based — `{startframe}{endframe}text` with `|` separating
//! text lines — so both parsing and serializing need the video frame rate to
//! convert frames↔milliseconds. A *leading* `{1}{1}fps` declaration line, if
//! present, is consumed and used as the rate.
//!
//! First-cut limitations: a consumed `{1}{1}fps` declaration is not re-emitted,
//! and `parse_line` trims surrounding whitespace from each record.

use crate::cue::{Cue, Format, Subtitle};

/// Fallback frame rate when none is provided or declared (NTSC film, 23.976).
pub const DEFAULT_FPS: f64 = 24_000.0 / 1_001.0;

/// A usable frame rate: positive and finite, else [`DEFAULT_FPS`].
fn sane_fps(fps: f64) -> f64 {
    if fps.is_finite() && fps > 0.0 {
        fps
    } else {
        DEFAULT_FPS
    }
}

/// Parse MicroDVD text. `fps` converts frames to milliseconds; a *leading*
/// `{1}{1}fps` declaration line in the file overrides it. Lines that don't match
/// the `{a}{b}text` shape are skipped.
pub fn parse(input: &str, fps: f64) -> Subtitle {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let normalized = input.replace("\r\n", "\n").replace('\r', "\n");

    let mut rate = sane_fps(fps);
    let mut cues = Vec::new();

    for line in normalized.lines() {
        let Some((start_frame, end_frame, text)) = parse_line(line) else {
            continue;
        };
        // Only a LEADING {1}{1}<number> line (before any cue) is a rate
        // declaration; the same shape later is a real one-frame cue.
        if cues.is_empty() && start_frame == 1 && end_frame == 1 {
            if let Ok(declared) = text.trim().parse::<f64>() {
                if declared.is_finite() && declared > 0.0 {
                    rate = declared;
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
        header: String::new(),
        cues,
    }
}

/// Serialize a [`Subtitle`] to MicroDVD. `fps` converts milliseconds to frames.
pub fn serialize(sub: &Subtitle, fps: f64) -> String {
    let rate = sane_fps(fps);
    let mut out = String::new();
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
    fn frame_one_record_after_a_cue_is_not_a_declaration() {
        // {1}{1}<number> appearing after a real cue is a normal cue, not fps.
        let sub = parse("{25}{50}Hi\n{1}{1}123\n", 25.0);
        assert_eq!(sub.cues.len(), 2);
        assert_eq!(sub.cues[1].payload, "123");
    }

    #[test]
    fn non_finite_fps_falls_back_to_default() {
        // Infinity passed the old `fps > 0.0` guard and collapsed times to 0.
        let sub = parse("{24}{48}Hi\n", f64::INFINITY);
        assert!(sub.cues[0].start_ms > 0);
    }
}
