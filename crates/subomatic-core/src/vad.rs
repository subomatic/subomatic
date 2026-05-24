// SPDX-License-Identifier: Apache-2.0
//! Voice-activity detection: turning a decoded audio signal into the reference
//! speech [`Span`]s the aligner consumes.
//!
//! [`Vad`] is the pluggable interface; [`EnergyVad`] is a dependency-free
//! energy-threshold default. A more accurate detector (e.g. a WebRTC-VAD port
//! such as `earshot`, or Silero) can implement the same trait without touching
//! the alignment engine.

use crate::Span;

/// Turns a mono PCM signal into speech-activity spans.
pub trait Vad {
    /// Detect speech [`Span`]s in mono `samples` (f32 in roughly `[-1.0, 1.0]`)
    /// sampled at `sample_rate` Hz.
    fn detect(&self, samples: &[f32], sample_rate: u32) -> Vec<Span>;
}

/// A simple energy-threshold voice-activity detector.
///
/// Frames the signal, marks frames whose RMS energy exceeds
/// `threshold_factor × mean energy` as speech, merges spans separated by gaps no
/// larger than `merge_gap_ms`, and drops spans shorter than `min_speech_ms`.
/// Crude but dependency-free and good enough to seed alignment.
///
/// Being a *relative* (mean-based) detector, it needs contrast between speech
/// and quieter regions; a uniformly loud signal produces no spans. Swap in a
/// model-based [`Vad`] when that matters.
#[derive(Clone, Copy, Debug)]
pub struct EnergyVad {
    /// Analysis frame size, in milliseconds.
    pub frame_ms: u32,
    /// A frame is speech when its RMS energy exceeds this multiple of the mean.
    pub threshold_factor: f32,
    /// Discard detected spans shorter than this, in milliseconds.
    pub min_speech_ms: i64,
    /// Bridge speech spans separated by a gap no larger than this, in milliseconds.
    pub merge_gap_ms: i64,
}

impl Default for EnergyVad {
    fn default() -> Self {
        EnergyVad {
            frame_ms: 20,
            threshold_factor: 1.5,
            min_speech_ms: 100,
            merge_gap_ms: 200,
        }
    }
}

impl Vad for EnergyVad {
    fn detect(&self, samples: &[f32], sample_rate: u32) -> Vec<Span> {
        if samples.is_empty() || sample_rate == 0 {
            return Vec::new();
        }
        let sample_rate = i64::from(sample_rate);
        let frame_len =
            (i64::from(self.frame_ms).saturating_mul(sample_rate) / 1000).max(1) as usize;
        let total_samples = samples.len() as i64;

        // RMS energy per frame; non-finite samples contribute no energy.
        let energies: Vec<f32> = samples
            .chunks(frame_len)
            .map(|frame| {
                let sum_sq: f32 = frame
                    .iter()
                    .map(|&s| if s.is_finite() { s * s } else { 0.0 })
                    .sum();
                (sum_sq / frame.len() as f32).sqrt()
            })
            .collect();

        let mean = energies.iter().sum::<f32>() / energies.len() as f32;
        let threshold = mean * self.threshold_factor;

        // Map a frame index to milliseconds via its sample position (drift-free,
        // and clamped to the real duration so the last partial frame can't run
        // past the end of the audio).
        let frame_to_ms = |frame_idx: usize| -> i64 {
            let sample = (frame_idx as i64)
                .saturating_mul(frame_len as i64)
                .min(total_samples);
            sample.saturating_mul(1000) / sample_rate
        };

        // Contiguous above-threshold frames become raw spans.
        let mut spans: Vec<Span> = Vec::new();
        let mut frame = 0usize;
        while frame < energies.len() {
            if energies[frame] > threshold {
                let start = frame;
                while frame < energies.len() && energies[frame] > threshold {
                    frame += 1;
                }
                spans.push(Span::new(frame_to_ms(start), frame_to_ms(frame)));
            } else {
                frame += 1;
            }
        }

        merge_and_filter(spans, self.merge_gap_ms, self.min_speech_ms)
    }
}

/// Merge ordered spans separated by a gap no larger than `merge_gap_ms`, then
/// drop any shorter than `min_speech_ms`.
fn merge_and_filter(spans: Vec<Span>, merge_gap_ms: i64, min_speech_ms: i64) -> Vec<Span> {
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start.saturating_sub(last.end) <= merge_gap_ms => {
                last.end = last.end.max(span.end);
            }
            _ => merged.push(span),
        }
    }
    merged.retain(|s| s.len() >= min_speech_ms);
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loud_region(sr: u32) -> Vec<f32> {
        let mut samples = vec![0.0f32; sr as usize * 2]; // 2 s of silence
                                                         // A loud region from 0.5 s to 1.5 s.
        for sample in samples.iter_mut().skip(sr as usize / 2).take(sr as usize) {
            *sample = 0.5;
        }
        samples
    }

    #[test]
    fn empty_or_zero_rate_yields_no_spans() {
        assert!(EnergyVad::default().detect(&[], 8_000).is_empty());
        assert!(EnergyVad::default().detect(&[0.1, 0.2], 0).is_empty());
    }

    #[test]
    fn detects_a_loud_region_against_silence() {
        let sr = 8_000u32;
        let spans = EnergyVad::default().detect(&loud_region(sr), sr);
        assert!(!spans.is_empty(), "no speech detected");
        let covered: i64 = spans.iter().map(Span::len).sum();
        assert!(
            (800..=1200).contains(&covered),
            "covered {covered} ms, spans {spans:?}"
        );
        assert!((400..=600).contains(&spans[0].start));
    }

    #[test]
    fn tolerates_non_finite_samples() {
        let sr = 8_000u32;
        let mut samples = loud_region(sr);
        samples[100] = f32::NAN;
        samples[200] = f32::INFINITY;
        // NaN/inf must not poison the mean/threshold and suppress all output.
        assert!(!EnergyVad::default().detect(&samples, sr).is_empty());
    }

    #[test]
    fn extreme_sample_rate_does_not_panic() {
        let _ = EnergyVad::default().detect(&[0.1, 0.2, 0.3], u32::MAX);
    }
}
