// SPDX-License-Identifier: Apache-2.0
//
// Worker that runs the WASM sync off the main thread, so the page stays
// responsive and a progress bar can animate while the (synchronous, possibly
// multi-second) alignment runs. Audio is decoded on the main thread — the Web
// Audio API isn't available in workers — and the PCM is transferred in here.

import init, { sync_to_reference, sync_to_audio } from "./pkg/subomatic_wasm.js";

// Start loading the WASM module immediately, so it's usually ready by the time
// the first job arrives.
const ready = init();

self.onmessage = async (event) => {
  const job = event.data;
  try {
    await ready;
    // Forwarded to Rust as `on_progress(stage, fraction)`; relay each tick to
    // the page. Rust already throttles these to ~1% steps per phase.
    const onProgress = (stage, fraction) =>
      self.postMessage({ type: "progress", stage, fraction });

    // `outFormat` is an extension string ("srt"/"vtt"/"sub"/"ass"); "" keeps the
    // input's format. `vad` is "energy" (fast) or "" / "earshot" (accurate).
    const outFormat = job.outFormat ?? "";
    const vad = job.vad ?? "";
    let result;
    if (job.mode === "audio") {
      const samples = new Float32Array(job.samples);
      result = sync_to_audio(job.subText, job.subFormat, samples, job.sampleRate, job.fps, outFormat, vad, onProgress);
    } else {
      result = sync_to_reference(job.subText, job.subFormat, job.refText, job.refFormat, job.fps, outFormat, onProgress);
    }
    self.postMessage({ type: "done", result });
  } catch (error) {
    self.postMessage({ type: "error", message: String(error?.message ?? error) });
  }
};
