// SPDX-License-Identifier: Apache-2.0
//
// Desktop (Tauri) front-end for Subomatic. The UI is identical to the web app;
// only the engine call differs: instead of loading WASM in a Web Worker, this
// invokes the native `subomatic-core` engine through Tauri commands
// (`sync_to_audio` / `sync_to_reference`) and listens for a `sync-progress`
// event to drive the progress bar.
//
// Audio is still decoded *in the webview* via WebAudio (there is no native
// libav/ffmpeg) and the resulting mono PCM is handed to the native command.
// Nothing leaves the machine.

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const runButton = document.getElementById("run");
const downloadButton = document.getElementById("download");
const statusEl = document.getElementById("status");
const progressEl = document.getElementById("progress");
const progressLabel = document.getElementById("progress-label");
const progressPct = document.getElementById("progress-pct");
const progressBar = document.getElementById("progress-bar");
const progressNote = document.getElementById("progress-note");

// Human labels for the engine's progress stages.
const STAGE_LABELS = {
  decode: "Decoding audio…",
  speech: "Detecting speech…",
  align: "Aligning subtitles…",
};

// The audio path has two phases; map each into one monotonic 0–1 bar so it
// never resets mid-run. Speech detection owns the first 45%, alignment the rest.
const SPEECH_SHARE = 0.45;

let elapsedTimer = null;
// The most recent successful sync, kept in memory so the manual download button
// can re-save it instantly (e.g. if the automatic download was cancelled)
// without re-decoding and re-aligning. Cleared/replaced on each new run.
let lastSynced = null;

function setStatus(message, isError = false) {
  statusEl.textContent = message;
  statusEl.classList.toggle("error", isError);
}

/** Show the progress panel and (re)start the elapsed-time ticker. */
function startProgress() {
  setStatus("");
  progressEl.classList.remove("hidden");
  progressEl.setAttribute("aria-hidden", "false");
  const startedAt = performance.now();
  stopElapsed();
  elapsedTimer = setInterval(() => {
    const secs = Math.round((performance.now() - startedAt) / 1000);
    progressNote.textContent = secs >= 3 ? `${secs}s elapsed` : "";
  }, 500);
}

function stopElapsed() {
  if (elapsedTimer !== null) {
    clearInterval(elapsedTimer);
    elapsedTimer = null;
  }
}

function hideProgress() {
  stopElapsed();
  progressEl.classList.add("hidden");
  progressEl.setAttribute("aria-hidden", "true");
  progressNote.textContent = "";
}

/** Indeterminate state: a stage we can't measure (decode), so animate, no %. */
function setIndeterminate(label) {
  progressLabel.textContent = label;
  progressPct.textContent = "";
  progressBar.style.width = "100%";
  progressEl.classList.add("indeterminate");
}

/** Determinate state: fill the bar to `fraction` (0–1) under `label`. */
function setDeterminate(label, fraction) {
  const pct = Math.max(0, Math.min(100, Math.round(fraction * 100)));
  progressEl.classList.remove("indeterminate");
  progressLabel.textContent = label;
  progressPct.textContent = `${pct}%`;
  progressBar.style.width = `${pct}%`;
}

/** Map an engine progress event to one monotonic fraction across both phases. */
function combinedFraction(mode, stage, fraction) {
  if (mode !== "audio") return fraction; // reference mode: alignment only
  return stage === "speech" ? fraction * SPEECH_SHARE : SPEECH_SHARE + fraction * (1 - SPEECH_SHARE);
}

/** Map a filename to a Subomatic format string (`.ssa` -> `ass`). */
function formatOf(name) {
  const dot = name.lastIndexOf(".");
  const ext = dot >= 0 ? name.slice(dot + 1).toLowerCase() : "";
  return ext === "ssa" ? "ass" : ext;
}

/**
 * Trigger a download of `text` as `<source>.synced.<ext>`. `outFormat` (an
 * extension without the dot, e.g. "vtt") overrides the extension when the output
 * format differs from the input; otherwise the source's own extension is kept.
 */
function download(text, sourceName, outFormat) {
  const dot = sourceName.lastIndexOf(".");
  const base = dot >= 0 ? sourceName.slice(0, dot) : sourceName;
  const ext = outFormat ? `.${outFormat}` : dot >= 0 ? sourceName.slice(dot) : ".srt";
  const url = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = `${base}.synced${ext}`;
  link.click();
  URL.revokeObjectURL(url);
}

/** Decode any browser-supported media file into a mono Float32 PCM signal. */
async function decodeToMono(file) {
  const bytes = await file.arrayBuffer();
  const Ctx = window.AudioContext || window["webkitAudioContext"];
  const ctx = new Ctx();
  try {
    const audio = await ctx.decodeAudioData(bytes);
    const mono = new Float32Array(audio.length);
    for (let c = 0; c < audio.numberOfChannels; c++) {
      const channel = audio.getChannelData(c);
      for (let i = 0; i < audio.length; i++) {
        mono[i] += channel[i] / audio.numberOfChannels;
      }
    }
    return { samples: mono, sampleRate: audio.sampleRate };
  } finally {
    await ctx.close();
  }
}

// Only one job runs at a time (the Sync button is disabled while busy), so a
// single in-flight handler is enough. We subscribe to the native engine's
// `sync-progress` event once and route ticks to the active job.
let activeJob = null;
listen("sync-progress", (event) => {
  if (!activeJob) return;
  const { stage, fraction } = event.payload;
  activeJob.onProgress(stage, fraction);
});

/**
 * Run a sync job natively. `job.mode` picks the command; the remaining fields
 * become the (camelCase) command arguments. Resolves with the synced subtitle
 * text, reporting progress via the shared `sync-progress` listener above.
 *
 * The `transfer` parameter is accepted for call-site parity with the web app
 * (which transfers the PCM buffer to a Worker); natively there's nothing to
 * transfer, so it's ignored.
 */
function runInWorker(job, transfer, onProgress) {
  activeJob = { onProgress };
  const finish = (p) => p.finally(() => { activeJob = null; });
  if (job.mode === "audio") {
    return finish(
      invoke("sync_to_audio", {
        input: job.subText,
        format: job.subFormat,
        samples: job.samples,
        sampleRate: job.sampleRate,
        fps: job.fps,
        outFormat: job.outFormat ?? "",
        vad: job.vad ?? "",
      }),
    );
  }
  return finish(
    invoke("sync_to_reference", {
      input: job.subText,
      format: job.subFormat,
      referenceText: job.refText,
      referenceFormat: job.refFormat,
      fps: job.fps,
      outFormat: job.outFormat ?? "",
    }),
  );
}

function selectedMode() {
  return document.querySelector('input[name="mode"]:checked').value;
}

function toggleMode() {
  const mode = selectedMode();
  document.getElementById("audio-field").classList.toggle("hidden", mode !== "audio");
  document.getElementById("reference-field").classList.toggle("hidden", mode !== "reference");
  updateFpsVisibility();
}

/**
 * The frame rate is only meaningful for MicroDVD `.sub`, which stores frame
 * numbers. Show the field only when a `.sub` is actually involved — as the
 * input subtitle, the reference (in reference mode), or the chosen output —
 * otherwise it's just noise.
 */
function needsFps() {
  const isSub = (input) => {
    const file = document.getElementById(input).files[0];
    return file && formatOf(file.name) === "sub";
  };
  if (isSub("subtitle")) return true;
  if (document.getElementById("output-format").value === "sub") return true;
  if (selectedMode() === "reference" && isSub("reference")) return true;
  return false;
}

function updateFpsVisibility() {
  document.getElementById("fps-field").classList.toggle("hidden", !needsFps());
}

async function run(event) {
  event.preventDefault();
  runButton.disabled = true;
  downloadButton.classList.add("hidden");
  startProgress();
  try {
    const subFile = document.getElementById("subtitle").files[0];
    if (!subFile) throw new Error("choose a subtitle to fix");
    const subText = await subFile.text();
    const subFormat = formatOf(subFile.name);
    const fps = Number.parseFloat(document.getElementById("fps").value) || 23.976;
    const mode = selectedMode();
    // "" means "same as input"; otherwise an extension like "vtt"/"ass".
    const outFormat = document.getElementById("output-format")?.value || "";
    // Speech detector for audio mode: "" / "earshot" (accurate) or "energy" (fast).
    const vad = document.getElementById("vad")?.value || "";

    const onProgress = (stage, fraction) =>
      setDeterminate(STAGE_LABELS[stage] ?? "Working…", combinedFraction(mode, stage, fraction));

    let result;
    if (mode === "audio") {
      const audioFile = document.getElementById("audio").files[0];
      if (!audioFile) throw new Error("choose a video or audio file");
      // Decode is async and offloaded by the browser, but it's unmeasurable, so
      // show an indeterminate bar (with an elapsed counter) until the engine
      // starts reporting real progress.
      setIndeterminate(STAGE_LABELS.decode);
      const { samples, sampleRate } = await decodeToMono(audioFile);
      setIndeterminate(STAGE_LABELS.speech);
      // The native command takes `samples: Vec<f32>`; hand it a plain array.
      const job = { mode, subText, subFormat, fps, outFormat, vad, samples: Array.from(samples), sampleRate };
      result = await runInWorker(job, [], onProgress);
    } else {
      const refFile = document.getElementById("reference").files[0];
      if (!refFile) throw new Error("choose a reference subtitle");
      const refText = await refFile.text();
      const refFormat = formatOf(refFile.name);
      setIndeterminate(STAGE_LABELS.align);
      const job = { mode, subText, subFormat, fps, outFormat, refText, refFormat };
      result = await runInWorker(job, [], onProgress);
    }

    // Cache the result so the manual button can re-save it without redoing the
    // (potentially slow) decode + alignment.
    lastSynced = { text: result, sourceName: subFile.name, outFormat };
    download(result, subFile.name, outFormat);
    hideProgress();
    downloadButton.classList.remove("hidden");
    setStatus("Done — your synced subtitle was downloaded.");
  } catch (error) {
    hideProgress();
    setStatus(`Error: ${error?.message ?? error}`, true);
  } finally {
    runButton.disabled = false;
  }
}

function main() {
  for (const radio of document.querySelectorAll('input[name="mode"]')) {
    radio.addEventListener("change", toggleMode);
  }
  // Re-evaluate whether the frame-rate field is relevant as inputs change.
  for (const id of ["subtitle", "reference", "output-format"]) {
    document.getElementById(id).addEventListener("change", updateFpsVisibility);
  }
  document.getElementById("sync-form").addEventListener("submit", run);
  // Re-save the cached result (no recompute) — e.g. if the auto-download was cancelled.
  downloadButton.addEventListener("click", () => {
    if (lastSynced) download(lastSynced.text, lastSynced.sourceName, lastSynced.outFormat);
  });
  toggleMode();
  setStatus("Ready.");
}

main();
