// SPDX-License-Identifier: Apache-2.0
//
// Browser front-end for Subomatic. Loads the WASM core and synchronizes a
// subtitle either to a reference subtitle or to a media file's speech (decoded
// in-browser via WebAudio). Nothing leaves the page.

import init, { sync_to_reference, sync_to_audio } from "./pkg/subomatic_wasm.js";

const statusEl = document.getElementById("status");
const runButton = document.getElementById("run");

function setStatus(message, isError = false) {
  statusEl.textContent = message;
  statusEl.classList.toggle("error", isError);
}

/** Map a filename to a Subomatic format string (`.ssa` -> `ass`). */
function formatOf(name) {
  const dot = name.lastIndexOf(".");
  const ext = dot >= 0 ? name.slice(dot + 1).toLowerCase() : "";
  return ext === "ssa" ? "ass" : ext;
}

/** Trigger a download of `text` next to the source filename, with `.synced`. */
function download(text, sourceName) {
  const dot = sourceName.lastIndexOf(".");
  const base = dot >= 0 ? sourceName.slice(0, dot) : sourceName;
  const ext = dot >= 0 ? sourceName.slice(dot) : ".srt";
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

function selectedMode() {
  return document.querySelector('input[name="mode"]:checked').value;
}

function toggleMode() {
  const mode = selectedMode();
  document.getElementById("audio-field").classList.toggle("hidden", mode !== "audio");
  document.getElementById("reference-field").classList.toggle("hidden", mode !== "reference");
}

async function run(event) {
  event.preventDefault();
  runButton.disabled = true;
  try {
    const subFile = document.getElementById("subtitle").files[0];
    if (!subFile) throw new Error("choose a subtitle to fix");
    const subText = await subFile.text();
    const subFormat = formatOf(subFile.name);
    const fps = Number.parseFloat(document.getElementById("fps").value) || 23.976;

    let result;
    if (selectedMode() === "audio") {
      const audioFile = document.getElementById("audio").files[0];
      if (!audioFile) throw new Error("choose a video or audio file");
      setStatus("Decoding audio…");
      const { samples, sampleRate } = await decodeToMono(audioFile);
      setStatus("Detecting speech and syncing…");
      result = sync_to_audio(subText, subFormat, samples, sampleRate, fps);
    } else {
      const refFile = document.getElementById("reference").files[0];
      if (!refFile) throw new Error("choose a reference subtitle");
      const refText = await refFile.text();
      setStatus("Syncing…");
      result = sync_to_reference(subText, subFormat, refText, formatOf(refFile.name), fps);
    }

    download(result, subFile.name);
    setStatus("Done — your synced subtitle has been downloaded.");
  } catch (error) {
    setStatus(`Error: ${error?.message ?? error}`, true);
  } finally {
    runButton.disabled = false;
  }
}

async function main() {
  setStatus("Loading…");
  await init();
  for (const radio of document.querySelectorAll('input[name="mode"]')) {
    radio.addEventListener("change", toggleMode);
  }
  document.getElementById("sync-form").addEventListener("submit", run);
  toggleMode();
  setStatus("Ready.");
}

main().catch((error) => setStatus(`Failed to start: ${error?.message ?? error}`, true));
