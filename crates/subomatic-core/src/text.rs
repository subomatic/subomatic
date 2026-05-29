// SPDX-License-Identifier: Apache-2.0
//! Shared low-level helpers for the subtitle text formats.
//!
//! All four parsers (`srt`, `vtt`, `ass`, `microdvd`) preprocess their input the
//! same way, and the three timestamped formats (`srt`, `vtt`, `ass`) share the
//! millisecond↔`H:MM:SS` arithmetic. Keeping that here means a change to BOM/line
//! handling, clamping, or overflow protection lands in every format at once.

/// Strip a leading UTF-8 BOM, then normalize CRLF/CR line endings to `\n`.
pub(crate) fn normalize(input: &str) -> String {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    input.replace("\r\n", "\n").replace('\r', "\n")
}

/// Split a millisecond count into `(hours, minutes, seconds, millis)`.
///
/// Negative inputs clamp to zero, so serializers never emit a negative timestamp.
pub(crate) fn decompose_ms(ms: i64) -> (i64, i64, i64, i64) {
    let ms = ms.max(0);
    (
        ms / 3_600_000,
        (ms % 3_600_000) / 60_000,
        (ms % 60_000) / 1000,
        ms % 1000,
    )
}

/// Combine `hours:minutes:seconds` plus an already-millisecond sub-second part
/// into total milliseconds, using checked arithmetic so malformed huge fields
/// return `None` instead of overflowing `i64`.
///
/// Callers convert their own sub-second unit first (e.g. ASS centiseconds ×10).
pub(crate) fn hms_to_ms(hours: i64, minutes: i64, seconds: i64, subsec_ms: i64) -> Option<i64> {
    hours
        .checked_mul(60)
        .and_then(|v| v.checked_add(minutes))
        .and_then(|v| v.checked_mul(60))
        .and_then(|v| v.checked_add(seconds))
        .and_then(|v| v.checked_mul(1000))
        .and_then(|v| v.checked_add(subsec_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_bom_and_line_endings() {
        assert_eq!(normalize("\u{feff}a\r\nb\rc"), "a\nb\nc");
        assert_eq!(normalize("plain"), "plain");
    }

    #[test]
    fn decompose_round_trips_and_clamps_negatives() {
        assert_eq!(decompose_ms(3_661_001), (1, 1, 1, 1));
        assert_eq!(decompose_ms(-5), (0, 0, 0, 0));
    }

    #[test]
    fn hms_to_ms_is_checked() {
        assert_eq!(hms_to_ms(1, 1, 1, 1), Some(3_661_001));
        assert_eq!(hms_to_ms(i64::MAX, 0, 0, 0), None);
    }
}
