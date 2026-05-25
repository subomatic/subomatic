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

/// A neural voice-activity detector backed by the pure-Rust [`earshot`] crate
/// (FFI-free, so the core stays WASM-clean). Sharper than [`EnergyVad`] on real
/// speech — it judges spectral shape, not just loudness — at the cost of one
/// dependency, so it lives behind the `earshot` feature.
///
/// `earshot` consumes 16 kHz mono frames of exactly 256 samples (16 ms), so the
/// input is resampled to 16 kHz first. Resampling preserves wall-clock time, so
/// the returned spans are still on the original timeline.
#[cfg(feature = "earshot")]
#[derive(Clone, Copy, Debug)]
pub struct EarshotVad {
    /// A 16 ms frame counts as speech when its score exceeds this. `earshot`
    /// scores in `[0, 1]`; 0.5 is the usual voice threshold.
    pub threshold: f32,
    /// Discard detected spans shorter than this, in milliseconds.
    pub min_speech_ms: i64,
    /// Bridge speech spans separated by a gap no larger than this, in milliseconds.
    pub merge_gap_ms: i64,
}

#[cfg(feature = "earshot")]
impl Default for EarshotVad {
    fn default() -> Self {
        EarshotVad {
            threshold: 0.5,
            min_speech_ms: 100,
            merge_gap_ms: 200,
        }
    }
}

#[cfg(feature = "earshot")]
impl Vad for EarshotVad {
    fn detect(&self, samples: &[f32], sample_rate: u32) -> Vec<Span> {
        // earshot's fixed frame: 256 samples = 16 ms at 16 kHz.
        const FRAME: usize = 256;
        const FRAME_MS: i64 = 16;
        if samples.is_empty() || sample_rate == 0 {
            return Vec::new();
        }

        let audio = resample_to_16k(samples, sample_rate);
        let mut detector = earshot::Detector::default();

        // Walk fixed frames, recording runs of above-threshold (speech) frames.
        // Frame `i` spans `[i, i+1) * FRAME_MS` on the original timeline.
        let mut spans: Vec<Span> = Vec::new();
        let mut run_start: Option<usize> = None;
        for (idx, frame) in audio.chunks_exact(FRAME).enumerate() {
            let speech = detector.predict_f32(frame) > self.threshold;
            match (speech, run_start) {
                (true, None) => run_start = Some(idx),
                (false, Some(start)) => {
                    spans.push(Span::new(start as i64 * FRAME_MS, idx as i64 * FRAME_MS));
                    run_start = None;
                }
                _ => {}
            }
        }
        if let Some(start) = run_start {
            // Extend a still-open run to the true end of the audio, so speech that
            // continues into the dropped final partial frame (< 16 ms) isn't
            // truncated at the last whole-frame boundary.
            let end_ms = audio.len() as i64 * 1000 / 16_000;
            spans.push(Span::new(start as i64 * FRAME_MS, end_ms));
        }

        merge_and_filter(spans, self.merge_gap_ms, self.min_speech_ms)
    }
}

/// Resample a mono f32 signal to 16 kHz by linear interpolation, sanitizing
/// non-finite samples and clamping to `[-1, 1]` (earshot's required range).
/// Quality is intentionally modest: a VAD only needs the speech envelope.
#[cfg(feature = "earshot")]
fn resample_to_16k(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    const TARGET: f64 = 16_000.0;
    let sanitize = |s: f32| {
        if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    if sample_rate == 16_000 {
        return samples.iter().map(|&s| sanitize(s)).collect();
    }
    let src_rate = f64::from(sample_rate);
    let out_len = ((samples.len() as f64) * TARGET / src_rate).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for j in 0..out_len {
        let pos = j as f64 * src_rate / TARGET;
        let i = pos.floor() as usize;
        let frac = (pos - i as f64) as f32;
        let a = samples.get(i).copied().map(sanitize).unwrap_or(0.0);
        let b = samples.get(i + 1).copied().map(sanitize).unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
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

#[cfg(all(test, feature = "earshot"))]
mod earshot_tests {
    use super::*;

    #[test]
    fn resample_halves_count_and_interpolates() {
        // 32 kHz -> 16 kHz halves the count; a ramp's output[j] tracks input[2j].
        let src: Vec<f32> = (0..32).map(|i| i as f32 / 64.0).collect();
        let out = resample_to_16k(&src, 32_000);
        assert_eq!(out.len(), 16);
        assert!((out[0] - src[0]).abs() < 1e-6);
        assert!((out[5] - src[10]).abs() < 1e-3);
    }

    #[test]
    fn resample_passthrough_at_16k_sanitizes() {
        // Out-of-range and non-finite inputs are clamped / zeroed for earshot.
        let out = resample_to_16k(&[2.0, -3.0, f32::NAN, 0.25], 16_000);
        assert_eq!(out, vec![1.0, -1.0, 0.0, 0.25]);
    }

    #[test]
    fn empty_or_zero_rate_yields_no_spans() {
        assert!(EarshotVad::default().detect(&[], 16_000).is_empty());
        assert!(EarshotVad::default().detect(&[0.1, 0.2], 0).is_empty());
    }

    #[test]
    fn silence_is_not_speech() {
        // One second of digital silence must not be flagged as voice.
        let silence = vec![0.0f32; 16_000];
        assert!(EarshotVad::default().detect(&silence, 16_000).is_empty());
    }

    #[test]
    fn tolerates_out_of_range_and_non_finite_without_panicking() {
        // earshot debug-asserts inputs are in [-1, 1]; sanitizing must keep it
        // happy on NaN/inf/overshoot, and the result must be valid (ordered) spans.
        let mut s = vec![0.0f32; 16_000];
        s[10] = f32::NAN;
        s[20] = 5.0;
        s[30] = f32::NEG_INFINITY;
        let spans = EarshotVad::default().detect(&s, 44_100);
        assert!(spans.iter().all(|sp| sp.end >= sp.start));
    }
}
