// SPDX-License-Identifier: Apache-2.0
//! The alignment engine.
//!
//! Both the reference (voice-activity spans, or a known-good subtitle's cues)
//! and the target subtitle are treated as sets of time [`Span`]s. Alignment
//! finds the timing correction that maximizes their overlap.
//!
//! One unified dynamic program does both jobs: [`align_offsets`] assigns each
//! cue an offset on a quantized grid, maximizing total overlap minus a
//! `split_penalty` per change of offset between consecutive cues.
//! `split_penalty = i64::MAX` forces a single shared offset (a global shift,
//! ffsubsync-style); smaller values allow piecewise shifts that absorb ad-breaks
//! or different cuts (alass-style). [`best_alignment`] wraps that in a scan over
//! common frame-rate ratios to also correct play-rate drift (e.g. 23.976↔25).

use crate::Span;

/// The range of offsets to search, in milliseconds.
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

/// Parameters for [`align_offsets`] / [`best_alignment`].
#[derive(Clone, Copy, Debug)]
pub struct AlignParams {
    pub range: SearchRange,
    /// Overlap (ms) charged each time consecutive cues take a different offset.
    /// `i64::MAX` forces one global shift; smaller values permit piecewise shifts
    /// (ad-breaks, different cuts). Tunable.
    pub split_penalty: i64,
}

impl Default for AlignParams {
    fn default() -> Self {
        // Default to a single global shift; lower `split_penalty` to allow splits.
        AlignParams {
            range: SearchRange::default(),
            split_penalty: i64::MAX,
        }
    }
}

/// The chosen warp: a global frame-rate scale plus a per-cue offset applied
/// after scaling, with the alignment's overlap score.
#[derive(Clone, Debug, PartialEq)]
pub struct Alignment {
    pub fps_ratio: f64,
    pub offsets: Vec<i64>,
    pub score: i64,
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

/// The offsets to evaluate, from `min_delta` to `max_delta` in `step` increments.
fn delta_grid(range: SearchRange) -> Vec<i64> {
    let step = range.step.max(1);
    let mut grid = Vec::new();
    let mut delta = range.min_delta;
    while delta <= range.max_delta {
        grid.push(delta);
        let Some(next) = delta.checked_add(step) else {
            break;
        };
        delta = next;
    }
    grid
}

/// Overlap (ms) between one `cue` shifted by `delta` and the (disjoint) reference.
fn overlap_at(reference: &[Span], cue: Span, delta: i64) -> i64 {
    let shifted = cue.shifted(delta);
    reference.iter().map(|r| shifted.overlap(r)).sum()
}

/// Total overlap (ms) of all `cues` shifted by a single `delta`.
fn total_overlap(reference: &[Span], cues: &[Span], delta: i64) -> i64 {
    cues.iter()
        .map(|&cue| overlap_at(reference, cue, delta))
        .sum()
}

/// Index of the highest score, breaking ties toward the offset closest to zero
/// so a no-evidence (all-equal) result lands on the smallest shift.
fn best_index(scores: &[i64], deltas: &[i64]) -> usize {
    let mut best = 0usize;
    for (i, (&score, &delta)) in scores.iter().zip(deltas).enumerate() {
        if score > scores[best]
            || (score == scores[best] && delta.unsigned_abs() < deltas[best].unsigned_abs())
        {
            best = i;
        }
    }
    best
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

/// Assign each cue an offset (ms, to add) via the piecewise dynamic program.
///
/// Returns the per-cue offsets and the alignment's score (total overlap minus
/// split penalties). See the module docs for the `split_penalty` knob.
pub fn align_offsets(reference: &[Span], cues: &[Span], params: &AlignParams) -> (Vec<i64>, i64) {
    if cues.is_empty() {
        return (Vec::new(), 0);
    }
    let reference = merge_spans(reference);
    let deltas = delta_grid(params.range);
    if reference.is_empty() || deltas.is_empty() {
        return (vec![0; cues.len()], 0);
    }

    // `dp[k]` = best score for cues so far with the current cue at `deltas[k]`.
    let mut dp: Vec<i64> = deltas
        .iter()
        .map(|&delta| overlap_at(&reference, cues[0], delta))
        .collect();
    // `back[i][k]` = delta-index chosen for cue `i`, given cue `i+1` sits at `k`.
    let mut back: Vec<Vec<usize>> = Vec::with_capacity(cues.len().saturating_sub(1));

    for &cue in &cues[1..] {
        let prev_best = best_index(&dp, &deltas);
        // Cost of arriving from a *different* previous offset (a split).
        let switch_score = dp[prev_best].saturating_sub(params.split_penalty);

        let mut row = vec![0i64; deltas.len()];
        let mut row_back = vec![0usize; deltas.len()];
        for (k, &delta) in deltas.iter().enumerate() {
            // Staying at the same offset (no penalty) vs. switching (penalty).
            let (from, base) = if dp[k] >= switch_score {
                (k, dp[k])
            } else {
                (prev_best, switch_score)
            };
            row[k] = overlap_at(&reference, cue, delta).saturating_add(base);
            row_back[k] = from;
        }
        dp = row;
        back.push(row_back);
    }

    // Backtrack from the best final cell.
    let mut k = best_index(&dp, &deltas);
    let score = dp[k];
    let mut offsets = vec![0i64; cues.len()];
    offsets[cues.len() - 1] = deltas[k];
    for i in (1..cues.len()).rev() {
        k = back[i - 1][k];
        offsets[i - 1] = deltas[k];
    }
    (offsets, score)
}

/// Common play-rate conversion ratios to test for frame-rate drift, using exact
/// NTSC fractions (23.976 = 24000/1001, 29.97 = 30000/1001).
fn fps_ratios() -> [f64; 9] {
    let film = 24_000.0 / 1_001.0; // 23.976
    let video = 30_000.0 / 1_001.0; // 29.97
    [
        1.0,
        25.0 / 24.0,
        24.0 / 25.0,
        24.0 / film,
        film / 24.0,
        25.0 / film,
        film / 25.0,
        30.0 / video,
        video / 30.0,
    ]
}

/// Scale a timestamp by a frame-rate `ratio` (saturating float→int cast).
pub fn scale_time(t: i64, ratio: f64) -> i64 {
    (t as f64 * ratio).round() as i64
}

/// Find the best [`Alignment`] (frame-rate ratio + per-cue offsets) by scanning
/// common play-rate ratios and running [`align_offsets`] for each.
pub fn best_alignment(reference: &[Span], cues: &[Span], params: &AlignParams) -> Alignment {
    let mut best = Alignment {
        fps_ratio: 1.0,
        offsets: vec![0; cues.len()],
        score: i64::MIN,
    };
    for ratio in fps_ratios() {
        let scaled: Vec<Span> = cues
            .iter()
            .map(|c| Span::new(scale_time(c.start, ratio), scale_time(c.end, ratio)))
            .collect();
        let (offsets, score) = align_offsets(reference, &scaled, params);
        if score > best.score {
            best = Alignment {
                fps_ratio: ratio,
                offsets,
                score,
            };
        }
    }
    best
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

        assert!(
            (delta + shift).abs() <= SearchRange::default().step,
            "expected ~{}, got {delta}",
            -shift
        );
    }

    #[test]
    fn empty_inputs_are_a_no_op() {
        assert_eq!(
            best_global_offset(&spans([]), &spans([(0, 1)]), SearchRange::default()),
            0
        );
        assert_eq!(
            best_global_offset(&spans([(0, 1)]), &spans([]), SearchRange::default()),
            0
        );
    }

    #[test]
    fn no_overlap_anywhere_returns_zero() {
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
        let range = SearchRange {
            min_delta: i64::MAX - 50,
            max_delta: i64::MAX,
            step: 100,
        };
        let _ = best_global_offset(&spans([(0, 1_000)]), &spans([(0, 1_000)]), range);
    }

    #[test]
    fn align_recovers_a_two_segment_split() {
        // First three cues are 1 s late; the last three are 4 s late (ad break).
        let reference = spans([
            (1_000, 2_000),
            (3_000, 4_000),
            (5_000, 6_000),
            (10_000, 11_000),
            (12_000, 13_000),
            (14_000, 15_000),
        ]);
        let cues: Vec<Span> = reference
            .iter()
            .enumerate()
            .map(|(i, r)| r.shifted(if i < 3 { 1_000 } else { 4_000 }))
            .collect();

        let params = AlignParams {
            range: SearchRange::default(),
            split_penalty: 200,
        };
        let (offsets, _) = align_offsets(&reference, &cues, &params);

        for (i, &off) in offsets.iter().enumerate() {
            let expected = if i < 3 { -1_000 } else { -4_000 };
            assert!(
                (off - expected).abs() <= SearchRange::default().step,
                "cue {i}: expected ~{expected}, got {off}"
            );
        }
    }

    #[test]
    fn align_with_infinite_penalty_is_a_global_shift() {
        let reference = spans([(1_000, 2_000), (5_000, 6_000), (9_000, 10_000)]);
        let cues: Vec<Span> = reference.iter().map(|s| s.shifted(1_500)).collect();

        let (offsets, _) = align_offsets(&reference, &cues, &AlignParams::default());

        assert!(offsets.iter().all(|&o| o == offsets[0]), "offsets differ");
        assert!((offsets[0] + 1_500).abs() <= SearchRange::default().step);
    }

    #[test]
    fn align_with_no_cues_is_empty() {
        let (offsets, score) =
            align_offsets(&spans([(0, 1_000)]), &spans([]), &AlignParams::default());
        assert!(offsets.is_empty());
        assert_eq!(score, 0);
    }

    #[test]
    fn best_alignment_recovers_an_fps_speedup() {
        let reference = spans([(1_000, 2_000), (60_000, 61_000), (3_600_000, 3_601_000)]);
        let film = 24_000.0 / 1_001.0; // 23.976
        let ratio = 25.0 / film; // the scale that restores the timing
                                 // Author cues as if mistimed at the other rate (compressed by 1/ratio).
        let cues: Vec<Span> = reference
            .iter()
            .map(|s| {
                Span::new(
                    (s.start as f64 / ratio).round() as i64,
                    (s.end as f64 / ratio).round() as i64,
                )
            })
            .collect();

        let alignment = best_alignment(&reference, &cues, &AlignParams::default());

        assert!(
            (alignment.fps_ratio - ratio).abs() < 1e-6,
            "expected ratio ~{ratio}, got {}",
            alignment.fps_ratio
        );
    }
}
