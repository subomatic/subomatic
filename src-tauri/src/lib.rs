// SPDX-License-Identifier: Apache-2.0
//! Subomatic desktop (Tauri) — a native front-end over the shared
//! `subomatic-core` engine. The two commands here mirror the WASM bindings
//! (`crates/subomatic-wasm`): align a subtitle to a reference subtitle, or to
//! decoded audio PCM the webview supplies (decoded in-browser via WebAudio —
//! there is deliberately no native libav/ffmpeg).
//!
//! Unlike the web build, the native app does **not** append the
//! "Synced with subomatic.github.io" attribution cue — it matches the CLI and
//! leaves files clean.
//!
//! Progress is reported by emitting a Tauri `"sync-progress"` event carrying
//! `{ stage, fraction }`; the frontend listens for it to drive its progress bar.
//! The heavy, synchronous work (voice detection + alignment) runs on a blocking
//! thread so it never stalls the webview's event loop.

use serde::Serialize;
use subomatic_core::{
    microdvd, sync_with_progress, AlignParams, EarshotVad, EnergyVad, Format, Span, Subtitle, Vad,
};
use tauri::{AppHandle, Emitter};

/// Payload for the `"sync-progress"` event the frontend listens for. `stage` is
/// `"speech"` (voice detection, audio mode only) or `"align"` (the timing
/// search); `fraction` runs `0.0..=1.0` within each stage.
#[derive(Clone, Serialize)]
struct Progress {
    stage: &'static str,
    fraction: f64,
}

/// Align `input` (format: `"srt"`/`"vtt"`/`"sub"`/`"ass"`/`"ssa"`) to a reference
/// subtitle's timings, returning the re-timed subtitle text.
///
/// `out_format` chooses the output format (`""` keeps the input's). Emits
/// `"sync-progress"` with `stage = "align"` as the alignment search advances.
#[tauri::command]
async fn sync_to_reference(
    app: AppHandle,
    input: String,
    format: String,
    reference_text: String,
    reference_format: String,
    fps: f64,
    out_format: String,
) -> Result<String, String> {
    // Off the webview's thread: the alignment search can run for seconds.
    tauri::async_runtime::spawn_blocking(move || {
        let mut report = throttle(&app);
        sync_to_reference_impl(
            &input,
            &format,
            &reference_text,
            &reference_format,
            fps,
            &out_format,
            &mut report,
        )
    })
    .await
    .map_err(|e| format!("sync task failed: {e}"))?
}

/// Align `input` to speech detected in mono PCM `samples` at `sample_rate` Hz
/// (the webview decodes the media via WebAudio), returning the re-timed subtitle.
///
/// `out_format` chooses the output format (`""` keeps the input's). `vad` selects
/// the detector: `"energy"` (fast, loudness-based) or anything else, including
/// `""`, for the neural `"earshot"` default. Emits `"sync-progress"` for the two
/// phases — `"speech"` then `"align"`.
// The frontend calls this with a flat positional-style arg list; keep them flat.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
async fn sync_to_audio(
    app: AppHandle,
    input: String,
    format: String,
    samples: Vec<f32>,
    sample_rate: u32,
    fps: f64,
    out_format: String,
    vad: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut report = throttle(&app);
        sync_to_audio_impl(
            &input,
            &format,
            &samples,
            sample_rate,
            fps,
            &out_format,
            &vad,
            &mut report,
        )
    })
    .await
    .map_err(|e| format!("sync task failed: {e}"))?
}

/// Wrap the Tauri event emit so it fires only on a stage change or after the
/// fraction advances by ~1% (and always at 1.0). The core's progress hooks tick
/// far more often than a UI needs; this keeps the event traffic to a handful of
/// updates per phase (mirrors the WASM layer's `throttle`).
fn throttle(app: &AppHandle) -> impl FnMut(&'static str, f64) + '_ {
    let mut last_stage: &'static str = "";
    let mut last_fraction = 0.0_f64;
    move |stage, fraction| {
        if stage != last_stage || fraction >= last_fraction + 0.01 || fraction >= 1.0 {
            last_stage = stage;
            last_fraction = fraction;
            let _ = app.emit("sync-progress", Progress { stage, fraction });
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
    let synced = sync_with_progress(
        &subtitle,
        &reference.spans(),
        &AlignParams::default(),
        &mut |f| progress("align", f),
    );
    // No attribution cue: the native app matches the CLI and leaves files clean.
    Ok(synced.serialize_as(out, fps))
}

#[allow(clippy::too_many_arguments)] // mirrors the flat front-end entry point
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
    let synced = sync_with_progress(&subtitle, &spans, &AlignParams::default(), &mut |f| {
        progress("align", f)
    });
    // No attribution cue (see `sync_to_reference_impl`).
    Ok(synced.serialize_as(out, fps))
}

/// Detect speech spans with the requested detector. `"energy"` selects the fast
/// loudness-based [`EnergyVad`]; anything else (the default) uses the neural
/// [`EarshotVad`], which is far more robust on music/effects-heavy audio.
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![sync_to_reference, sync_to_audio])
        .run(tauri::generate_context!())
        .expect("error while running Subomatic desktop");
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
    fn native_output_has_no_attribution_credit() {
        // The native app matches the CLI: no "Synced with subomatic.github.io".
        let out = sync_to_reference_impl(A_CUE, "srt", REF, "srt", 25.0, "", &mut ignore).unwrap();
        assert!(
            !out.contains("subomatic.github.io"),
            "credit must NOT be appended natively: {out}"
        );
    }

    #[test]
    fn audio_mode_detects_and_shifts() {
        let sr = 8_000u32;
        let mut samples = vec![0.0f32; sr as usize * 2];
        for s in samples.iter_mut().skip(sr as usize / 2).take(sr as usize) {
            *s = 0.5;
        }
        let late = "1\n00:00:05,000 --> 00:00:06,000\nHi\n";
        let out =
            sync_to_audio_impl(late, "srt", &samples, sr, 25.0, "", "energy", &mut ignore).unwrap();
        assert!(out.contains("00:00:00,500"), "got {out}");
    }

    #[test]
    fn output_format_can_differ_from_input() {
        let out =
            sync_to_reference_impl(A_CUE, "srt", REF, "srt", 25.0, "vtt", &mut ignore).unwrap();
        assert!(out.starts_with("WEBVTT"), "expected VTT output, got {out}");
    }

    #[test]
    fn rejects_bad_fps_and_sample_rate() {
        assert!(sync_to_reference_impl(A_CUE, "srt", REF, "srt", 0.0, "", &mut ignore).is_err());
        assert!(sync_to_audio_impl(A_CUE, "srt", &[0.1, 0.2], 0, 25.0, "", "energy", &mut ignore)
            .is_err());
    }
}
