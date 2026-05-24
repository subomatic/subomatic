// SPDX-License-Identifier: Apache-2.0
//! WebAssembly bindings for `subomatic-core`.
//!
//! Exposes subtitle synchronization to JavaScript: align to a reference
//! subtitle (sub-to-sub), or to decoded audio PCM the browser supplies (e.g.
//! via the WebAudio API or ffmpeg.wasm). The heavy lifting stays in the shared
//! Rust core; this layer is just thin glue.
//!
//! Subtitle `format` strings are `"srt"`, `"vtt"`, or `"sub"` (case-insensitive).

use subomatic_core::{microdvd, srt, sync, vtt, AlignParams, EnergyVad, Format, Subtitle, Vad};
use wasm_bindgen::prelude::*;

/// Align `input` (format: `"srt"`, `"vtt"`, or `"sub"`) to a reference
/// subtitle's timings, returning the re-timed subtitle in the same format.
#[wasm_bindgen]
pub fn sync_to_reference(
    input: &str,
    format: &str,
    reference_text: &str,
    reference_format: &str,
    fps: f64,
) -> Result<String, JsValue> {
    sync_to_reference_impl(input, format, reference_text, reference_format, fps)
        .map_err(JsValue::from)
}

/// Align `input` to speech detected in mono PCM `samples` at `sample_rate` Hz
/// (the caller decodes the audio in the browser), returning the re-timed
/// subtitle in the same format.
#[wasm_bindgen]
pub fn sync_to_audio(
    input: &str,
    format: &str,
    samples: &[f32],
    sample_rate: u32,
    fps: f64,
) -> Result<String, JsValue> {
    sync_to_audio_impl(input, format, samples, sample_rate, fps).map_err(JsValue::from)
}

fn sync_to_reference_impl(
    input: &str,
    format: &str,
    reference_text: &str,
    reference_format: &str,
    fps: f64,
) -> Result<String, String> {
    check_fps(fps)?;
    let subtitle = parse(input, format, fps)?;
    let reference = parse(reference_text, reference_format, fps)?;
    let synced = sync(&subtitle, &reference.spans(), &AlignParams::default());
    Ok(serialize(&synced, fps))
}

fn sync_to_audio_impl(
    input: &str,
    format: &str,
    samples: &[f32],
    sample_rate: u32,
    fps: f64,
) -> Result<String, String> {
    check_fps(fps)?;
    if sample_rate == 0 {
        return Err("sample_rate must be greater than 0".to_string());
    }
    let subtitle = parse(input, format, fps)?;
    let spans = EnergyVad::default().detect(samples, sample_rate);
    let synced = sync(&subtitle, &spans, &AlignParams::default());
    Ok(serialize(&synced, fps))
}

fn check_fps(fps: f64) -> Result<(), String> {
    if fps.is_finite() && fps > 0.0 {
        Ok(())
    } else {
        Err(format!("fps must be positive and finite, got {fps}"))
    }
}

fn parse(text: &str, format: &str, fps: f64) -> Result<Subtitle, String> {
    if format.eq_ignore_ascii_case("srt") {
        srt::parse(text).map_err(|e| e.to_string())
    } else if format.eq_ignore_ascii_case("vtt") {
        vtt::parse(text).map_err(|e| e.to_string())
    } else if format.eq_ignore_ascii_case("sub") {
        Ok(microdvd::parse(text, fps))
    } else {
        Err(format!("unsupported subtitle format: {format:?}"))
    }
}

fn serialize(subtitle: &Subtitle, fps: f64) -> String {
    match subtitle.format {
        Format::SubRip => srt::serialize(subtitle),
        Format::WebVtt => vtt::serialize(subtitle),
        Format::MicroDvd => microdvd::serialize(subtitle, fps),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_CUE: &str = "1\n00:00:04,000 --> 00:00:05,000\nHi\n";
    const REF: &str = "1\n00:00:01,000 --> 00:00:02,000\nHi\n";

    #[test]
    fn sub_to_sub_shifts_to_reference() {
        let out = sync_to_reference_impl(A_CUE, "srt", REF, "srt", 25.0).unwrap();
        assert!(out.contains("00:00:01,000 --> 00:00:02,000"), "got {out}");
    }

    #[test]
    fn audio_mode_detects_and_shifts() {
        let sr = 8_000u32;
        let mut samples = vec![0.0f32; sr as usize * 2];
        for s in samples.iter_mut().skip(sr as usize / 2).take(sr as usize) {
            *s = 0.5;
        }
        let late = "1\n00:00:05,000 --> 00:00:06,000\nHi\n";
        let out = sync_to_audio_impl(late, "srt", &samples, sr, 25.0).unwrap();
        // The loud region is ~0.5–1.5 s; the 1 s cue should land there.
        assert!(out.contains("00:00:00,500"), "got {out}");
    }

    #[test]
    fn unknown_format_errors() {
        assert!(sync_to_reference_impl("x", "ass", "y", "srt", 25.0).is_err());
    }

    #[test]
    fn rejects_bad_fps_and_sample_rate() {
        assert!(sync_to_reference_impl(A_CUE, "srt", REF, "srt", f64::NAN).is_err());
        assert!(sync_to_reference_impl(A_CUE, "srt", REF, "srt", 0.0).is_err());
        assert!(sync_to_audio_impl(A_CUE, "srt", &[0.1, 0.2], 0, 25.0).is_err());
    }
}
