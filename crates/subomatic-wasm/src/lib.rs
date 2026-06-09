// SPDX-License-Identifier: Apache-2.0
//! WebAssembly bindings for `subomatic-core`.
//!
//! Exposes subtitle synchronization to JavaScript: align to a reference
//! subtitle (sub-to-sub), or to decoded audio PCM the browser supplies (e.g.
//! via the WebAudio API or ffmpeg.wasm). The heavy lifting stays in the shared
//! Rust core; this layer is just thin glue.
//!
//! Subtitle `format` strings are `"srt"`, `"vtt"`, `"sub"`, `"ass"`, or `"ssa"`
//! (case-insensitive).

use subomatic_core::{
    microdvd, sync_with_progress, AlignParams, EarshotVad, EnergyVad, Format, Span, Subtitle, Vad,
};
use wasm_bindgen::prelude::*;

/// A short attribution cue appended to every synced subtitle the web app
/// produces (the native CLI, which calls the core directly, leaves files clean).
const CREDIT_TEXT: &str = "Synced with subomatic.github.io";
/// Credit starts this long after the last cue ends, and plays for this long.
const CREDIT_GAP_MS: i64 = 1_000;
const CREDIT_DURATION_MS: i64 = 3_000;

/// Align `input` (format: `"srt"`, `"vtt"`, or `"sub"`) to a reference
/// subtitle's timings, returning the re-timed subtitle.
///
/// `out_format` chooses the output format (`"srt"`/`"vtt"`/`"sub"`/`"ass"`/
/// `"ssa"`); pass `""` to keep the input's format. `on_progress(stage,
/// fraction)` is called as the work advances — `stage` is `"align"`, `fraction`
/// runs `0.0..=1.0` — so the page can show a real progress bar.
#[wasm_bindgen]
pub fn sync_to_reference(
    input: &str,
    format: &str,
    reference_text: &str,
    reference_format: &str,
    fps: f64,
    out_format: &str,
    on_progress: &js_sys::Function,
) -> Result<String, JsValue> {
    let mut report = throttle(on_progress);
    sync_to_reference_impl(
        input,
        format,
        reference_text,
        reference_format,
        fps,
        out_format,
        &mut report,
    )
    .map_err(JsValue::from)
}

/// Align `input` to speech detected in mono PCM `samples` at `sample_rate` Hz
/// (the caller decodes the audio in the browser), returning the re-timed
/// subtitle.
///
/// `out_format` chooses the output format (`""` keeps the input's). `vad`
/// chooses the speech detector: `"energy"` for the fast loudness-based detector,
/// anything else (including `""`) for the sharper neural `"earshot"` default —
/// energy misfires on music/effects-heavy audio. `on_progress(stage, fraction)`
/// reports the two phases — `"speech"` (voice detection) then `"align"` (the
/// timing search) — each with a `0.0..=1.0` `fraction`, so the page can keep a
/// progress bar moving while the heavy work runs off the main thread (in a Web
/// Worker).
// The JS API is positional, so the parameters can't be bundled into a struct.
#[allow(clippy::too_many_arguments)]
#[wasm_bindgen]
pub fn sync_to_audio(
    input: &str,
    format: &str,
    samples: &[f32],
    sample_rate: u32,
    fps: f64,
    out_format: &str,
    vad: &str,
    on_progress: &js_sys::Function,
) -> Result<String, JsValue> {
    let mut report = throttle(on_progress);
    sync_to_audio_impl(
        input,
        format,
        samples,
        sample_rate,
        fps,
        out_format,
        vad,
        &mut report,
    )
    .map_err(JsValue::from)
}

/// Wrap a JS `(stage, fraction)` callback so it fires only on a stage change or
/// after the fraction advances by ~1% (and always at 1.0). Rust's progress
/// hooks tick far more often than a UI needs; this keeps `postMessage` traffic
/// from the worker to a handful of updates per phase.
fn throttle(on_progress: &js_sys::Function) -> impl FnMut(&'static str, f64) + '_ {
    let mut last_stage: &'static str = "";
    let mut last_fraction = 0.0_f64;
    move |stage, fraction| {
        if stage != last_stage || fraction >= last_fraction + 0.01 || fraction >= 1.0 {
            last_stage = stage;
            last_fraction = fraction;
            let _ = on_progress.call2(
                &JsValue::NULL,
                &JsValue::from_str(stage),
                &JsValue::from_f64(fraction),
            );
        }
    }
}

fn sync_to_reference_impl(
    input: &str,
    format: &str,
    reference_text: &str,
    reference_format: &str,
    fps: f64,
    out_format: &str,
    progress: &mut dyn FnMut(&'static str, f64),
) -> Result<String, String> {
    check_fps(fps)?;
    let subtitle = parse(input, format, fps)?;
    let out = resolve_out_format(out_format, subtitle.format)?;
    let reference = parse(reference_text, reference_format, fps)?;
    let mut synced = sync_with_progress(
        &subtitle,
        &reference.spans(),
        &AlignParams::default(),
        &mut |f| progress("align", f),
    );
    synced.append_credit(CREDIT_TEXT, CREDIT_GAP_MS, CREDIT_DURATION_MS);
    Ok(synced.serialize_as(out, fps))
}

#[allow(clippy::too_many_arguments)] // mirrors the flat JS-facing entry point
fn sync_to_audio_impl(
    input: &str,
    format: &str,
    samples: &[f32],
    sample_rate: u32,
    fps: f64,
    out_format: &str,
    vad: &str,
    progress: &mut dyn FnMut(&'static str, f64),
) -> Result<String, String> {
    check_fps(fps)?;
    if sample_rate == 0 {
        return Err("sample_rate must be greater than 0".to_string());
    }
    let subtitle = parse(input, format, fps)?;
    let out = resolve_out_format(out_format, subtitle.format)?;
    let spans = detect_speech(vad, samples, sample_rate, &mut |f| progress("speech", f));
    let mut synced = sync_with_progress(&subtitle, &spans, &AlignParams::default(), &mut |f| {
        progress("align", f)
    });
    synced.append_credit(CREDIT_TEXT, CREDIT_GAP_MS, CREDIT_DURATION_MS);
    Ok(synced.serialize_as(out, fps))
}

/// Detect speech spans with the requested detector. `"energy"` selects the fast
/// loudness-based [`EnergyVad`]; anything else (the default) uses the neural
/// [`EarshotVad`], which is far more robust on music/effects-heavy audio where
/// loudness alone mistakes the soundtrack for speech.
fn detect_speech(
    vad: &str,
    samples: &[f32],
    sample_rate: u32,
    progress: &mut dyn FnMut(f64),
) -> Vec<Span> {
    if vad == "energy" {
        EnergyVad::default().detect_with_progress(samples, sample_rate, progress)
    } else {
        EarshotVad::default().detect_with_progress(samples, sample_rate, progress)
    }
}

/// Resolve the requested output format string, falling back to `default` (the
/// input's format) when it's empty. An unrecognized value is an error.
fn resolve_out_format(out_format: &str, default: Format) -> Result<Format, String> {
    if out_format.is_empty() {
        Ok(default)
    } else {
        Format::from_extension(out_format)
            .ok_or_else(|| format!("unsupported output format: {out_format:?}"))
    }
}

fn check_fps(fps: f64) -> Result<(), String> {
    if microdvd::is_valid_fps(fps) {
        Ok(())
    } else {
        Err(microdvd::invalid_fps_message(fps))
    }
}

fn parse(text: &str, format: &str, fps: f64) -> Result<Subtitle, String> {
    let format = Format::from_extension(format)
        .ok_or_else(|| format!("unsupported subtitle format: {format:?}"))?;
    Subtitle::parse(format, text, fps).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const A_CUE: &str = "1\n00:00:04,000 --> 00:00:05,000\nHi\n";
    const REF: &str = "1\n00:00:01,000 --> 00:00:02,000\nHi\n";

    /// A progress sink that ignores updates, for tests not asserting on them.
    fn ignore(_stage: &'static str, _fraction: f64) {}

    #[test]
    fn sub_to_sub_shifts_to_reference() {
        let out = sync_to_reference_impl(A_CUE, "srt", REF, "srt", 25.0, "", &mut ignore).unwrap();
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
        // Energy VAD: the synthetic tone is loud, not speech-shaped, so earshot
        // wouldn't flag it.
        let out =
            sync_to_audio_impl(late, "srt", &samples, sr, 25.0, "", "energy", &mut ignore).unwrap();
        // The loud region is ~0.5–1.5 s; the 1 s cue should land there.
        assert!(out.contains("00:00:00,500"), "got {out}");
    }

    #[test]
    fn audio_mode_reports_both_phases() {
        let sr = 8_000u32;
        let mut samples = vec![0.0f32; sr as usize * 2];
        for s in samples.iter_mut().skip(sr as usize / 2).take(sr as usize) {
            *s = 0.5;
        }
        let mut stages: Vec<&'static str> = Vec::new();
        sync_to_audio_impl(
            A_CUE,
            "srt",
            &samples,
            sr,
            25.0,
            "",
            "energy",
            &mut |stage, frac| {
                assert!((0.0..=1.0).contains(&frac), "fraction out of range: {frac}");
                if stages.last() != Some(&stage) {
                    stages.push(stage);
                }
            },
        )
        .unwrap();
        // Speech detection runs first, then alignment — in that order.
        assert_eq!(
            stages,
            ["speech", "align"],
            "phases out of order: {stages:?}"
        );
    }

    #[test]
    fn output_format_can_differ_from_input() {
        // Sub-to-sub with an SRT input but VTT requested out: VTT signature, no
        // SRT cue indices, timing still corrected to the reference.
        let out =
            sync_to_reference_impl(A_CUE, "srt", REF, "srt", 25.0, "vtt", &mut ignore).unwrap();
        assert!(out.starts_with("WEBVTT"), "expected VTT output, got {out}");
        assert!(out.contains("00:00:01.000 --> 00:00:02.000"), "got {out}");
    }

    #[test]
    fn unknown_input_and_output_formats_error() {
        assert!(sync_to_reference_impl("x", "ass", "y", "srt", 25.0, "", &mut ignore).is_err());
        // A valid input but an unrecognized output format is rejected too.
        assert!(
            sync_to_reference_impl(A_CUE, "srt", REF, "srt", 25.0, "xyz", &mut ignore).is_err()
        );
    }

    #[test]
    fn rejects_bad_fps_and_sample_rate() {
        assert!(
            sync_to_reference_impl(A_CUE, "srt", REF, "srt", f64::NAN, "", &mut ignore).is_err()
        );
        assert!(sync_to_reference_impl(A_CUE, "srt", REF, "srt", 0.0, "", &mut ignore).is_err());
        assert!(sync_to_audio_impl(
            A_CUE,
            "srt",
            &[0.1, 0.2],
            0,
            25.0,
            "",
            "energy",
            &mut ignore
        )
        .is_err());
    }

    #[test]
    fn output_carries_the_attribution_credit() {
        // The web output appends the credit just after the last cue.
        let out = sync_to_reference_impl(A_CUE, "srt", REF, "srt", 25.0, "", &mut ignore).unwrap();
        assert!(out.contains(CREDIT_TEXT), "credit missing: {out}");
        // It plays after the (shifted) dialogue, not over it: last cue ends at
        // 00:00:02,000, so the credit runs 00:00:03,000 -> 00:00:06,000.
        assert!(out.contains("00:00:03,000 --> 00:00:06,000"), "got {out}");
    }

    #[test]
    fn earshot_is_the_default_vad() {
        // A short, near-silent clip: earshot (the default) finds no speech, so the
        // subtitle is returned unshifted rather than chasing loudness. The point is
        // that an empty `vad` selects earshot without panicking.
        let sr = 16_000u32;
        let samples = vec![0.0f32; sr as usize];
        let out =
            sync_to_audio_impl(A_CUE, "srt", &samples, sr, 25.0, "", "", &mut ignore).unwrap();
        assert!(out.contains("00:00:04,000 --> 00:00:05,000"), "got {out}");
    }
}
