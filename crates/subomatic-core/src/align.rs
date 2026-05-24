// SPDX-License-Identifier: Apache-2.0
//! The alignment engine.
//!
//! Both the reference (voice-activity spans, or a known-good subtitle's cues)
//! and the target subtitle are treated as sets of time [`Span`]s. Alignment
//! finds the timing correction that maximizes their overlap.
//!
//! This first cut implements the global-shift case — the `split_penalty = ∞`
//! degenerate of the planned piecewise dynamic program (equivalent to what
//! ffsubsync computes). Piecewise splits and the frame-rate scan come next; see
//! `DESIGN.md`.

use crate::Span;

/// The range of global offsets to search, in milliseconds.
#[derive(Clone, Copy, Debug)]
pub struct SearchRange {
    pub min_delta: i64,
    pub max_delta: i64,
    pub step: i64,
}

impl Default for SearchRange {
    fn default() -> Self {
        // ±60 s at 20 ms resolution: covers typical real-world offsets.
        SearchRange {
            min_delta: -60_000,
            max_delta: 60_000,
            step: 20,
        }
    }
}

/// Merge spans into sorted, disjoint intervals (overlapping or touching spans are
/// combined) so overlap is measured against their union rather than
/// double-counted where reference spans overlap.
fn merge_spans(spans: &[Span]) -> Vec<Span> {
    let mut sorted: Vec<Span> = spans.iter().copied().filter(|s| !s.is_empty()).collect();
    sorted.sort_by_key(|s| s.start);

    let mut merged: Vec<Span> = Vec::with_capacity(sorted.len());
    for span in sorted {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

/// Total overlap (ms) between the `cues` shifted by `delta` and the `reference`.
///
/// `reference` is expected to be a set of disjoint spans (see [`merge_spans`]).
/// Kept deliberately simple (a direct double loop); the prefix-sum optimization
/// noted in `DESIGN.md` is only worth adding if profiling ever shows a need.
fn total_overlap(reference: &[Span], cues: &[Span], delta: i64) -> i64 {
    cues.iter()
        .map(|cue| {
            let shifted = cue.shifted(delta);
            reference.iter().map(|r| shifted.overlap(r)).sum::<i64>()
        })
        .sum()
}

/// Find the single global offset (ms, to add to every cue) that best aligns the
/// subtitle to the reference activity.
///
/// Returns `0` when there is nothing to align (no cues or no reference) or when
/// no offset produces any overlap — it never shifts subtitles without evidence.
/// Ties are broken toward the offset closest to zero.
pub fn best_global_offset(reference: &[Span], cues: &[Span], range: SearchRange) -> i64 {
    if reference.is_empty() || cues.is_empty() {
        return 0;
    }

    // Measure overlap against the union of the reference spans.
    let reference = merge_spans(reference);

    let step = range.step.max(1);
    // Seed with the no-op (delta 0): a candidate only wins with strictly more
    // overlap, and ties prefer the smaller absolute shift. With no overlap
    // anywhere, the result stays at 0.
    let mut best_delta: i64 = 0;
    let mut best_score = total_overlap(&reference, cues, 0);

    let mut delta = range.min_delta;
    while delta <= range.max_delta {
        let score = total_overlap(&reference, cues, delta);
        if score > best_score
            || (score == best_score && delta.unsigned_abs() < best_delta.unsigned_abs())
        {
            best_score = score;
            best_delta = delta;
        }
        // Checked add so a search range near i64::MAX can't overflow or loop forever.
        let Some(next) = delta.checked_add(step) else {
            break;
        };
        delta = next;
    }

    best_delta
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans<const N: usize>(intervals: [(i64, i64); N]) -> Vec<Span> {
        intervals.iter().map(|&(a, b)| Span::new(a, b)).collect()
    }

    #[test]
    fn recovers_a_known_global_shift() {
        let reference = spans([(1_000, 2_000), (5_000, 6_000), (9_000, 10_000)]);
        let shift = 1_500;
        let cues: Vec<Span> = reference.iter().map(|s| s.shifted(shift)).collect();

        let delta = best_global_offset(&reference, &cues, SearchRange::default());

        // The cues are 1.5 s late, so the correction must be about -1.5 s.
        assert!(
            (delta + shift).abs() <= SearchRange::default().step,
            "expected ~{}, got {delta}",
            -shift
        );
    }

    #[test]
    fn empty_inputs_are_a_no_op() {
        assert_eq!(
            best_global_offset(&[], &spans([(0, 1)]), SearchRange::default()),
            0
        );
        assert_eq!(
            best_global_offset(&spans([(0, 1)]), &[], SearchRange::default()),
            0
        );
    }

    #[test]
    fn no_overlap_anywhere_returns_zero() {
        // The cue can never reach the reference within the search window, so the
        // engine must report "no shift" rather than parking at `min_delta`.
        let reference = spans([(0, 1_000)]);
        let cues = spans([(500_000, 501_000)]);
        assert_eq!(
            best_global_offset(&reference, &cues, SearchRange::default()),
            0
        );
    }

    #[test]
    fn duplicate_reference_spans_do_not_bias_result() {
        let reference = spans([(1_000, 2_000), (1_000, 2_000)]);
        let cues = spans([(1_000, 2_000)]);
        assert_eq!(
            best_global_offset(&reference, &cues, SearchRange::default()),
            0
        );
    }

    #[test]
    fn merge_spans_unions_overlapping_intervals() {
        let merged = merge_spans(&spans([(0, 100), (50, 150), (200, 250)]));
        assert_eq!(merged, spans([(0, 150), (200, 250)]));
    }

    #[test]
    fn search_range_near_i64_max_terminates() {
        // The loop's checked increment must stop instead of overflowing or hanging.
        let range = SearchRange {
            min_delta: i64::MAX - 50,
            max_delta: i64::MAX,
            step: 100,
        };
        let _ = best_global_offset(&spans([(0, 1_000)]), &spans([(0, 1_000)]), range);
    }
}
