# Subomatic — Design & Decisions

Fully-automatic subtitle-sync app for the web (WASM), the Mac App Store, and
Windows (arm64/x64). This document is the durable source of truth (it mirrors the
project's working memory) so a lost chat can't erase the design again.
_Last updated: 2026-05-24._

## Principles

- **Maintainability over performance.** Implement the clearest correct version
  first; optimize only when profiling demands it.
- The core is **pure Rust, `#![forbid(unsafe_code)]`, with no platform/audio
  deps**, so it compiles unchanged to native and WASM.
- **Open source under Apache-2.0** (engine + apps), with a `NOTICE` file so
  reuse must carry attribution.

## Wedge

Nothing else does frictionless, fully-automatic *audio-based* fetch + sync for a
regular user: App Store tools are manual (SubShifter / Tap2Sync), off-store
auto-syncers make you supply files (AutoSubSync), and subsync.online — the one
in-browser audio syncer — is abandonware.

## Architecture

One shared Rust core plus a thin decode layer; the core has zero audio/platform
dependencies.

- **`subomatic-core`** (Rust): subtitle parse/serialize + VAD + the alignment DP
  + timing warp. The engine works on *interval sets*, so its real input is
  reference activity spans + subtitle cues (PCM → spans happens in the
  VAD/decode layer, outside the core).
- **Decode lives outside the core**, in two thin adapters that hand it PCM: the
  **CLI FFI-links libav** (libavcodec/libavformat, via the `ffmpeg-the-third`
  bindings); the **web app uses the browser's WebAudio** decoder. We *link* the
  library — we never shell out to the `ffmpeg` binary — so there's no external
  tool to find and no CLI surface that can change underneath us.
- **OpenSubtitles** fetch (REST) is wired into the CLI as `subomatic fetch`.

## Decode strategy

- Sync only needs a mono ~8–16 kHz **speech envelope**, so the CLI hands the file
  to libav, takes the default audio track, downmixes it to mono f32 (libav's
  resampler), and feeds the VAD. The engine never sees the container.
- **Link the library, don't shell out.** We FFI-link libavcodec/libavformat (via
  the `ffmpeg-the-third` bindings) instead of invoking the `ffmpeg` *binary*: a
  compiled API can't break on a CLI-flag change, and there's no separate tool to
  locate at runtime.
- **Coverage: everything libav decodes** — AAC, AC-3, DTS, E-AC-3, MP3, FLAC,
  Opus, Vorbis, ALAC, PCM, … inside MP4/MKV/TS/OGG/WAV/…. Verified end-to-end on
  AC-3 and DTS, the common codecs that ruled out the pure-Rust decoders.
- **Licensing:** libav is **LGPL-2.1+**. We link it **dynamically** (a system or
  bundled shared library), build it `--disable-gpl`, and credit it in `NOTICE`
  with a pointer to its source — which keeps our own code Apache-2.0 and is
  App-Store-shippable (the store conflict is with *GPL*, not LGPL via dynamic
  linking). For AC-3/DTS the software side is clean and the core patents have
  largely expired; an IP review before a *paid* launch is still prudent.
- **Trade-off we accepted:** libav is a C dependency, so the CLI needs libav +
  libclang at build time and doesn't compile to WASM. That's fine — no shipping
  target needs a Rust decoder in WASM: the **web app decodes with WebAudio** (the
  wasm crate only receives PCM), and `subomatic-core` stays pure-Rust/WASM-clean.

## Subtitle formats

- Timing-only: model = ordered cues `{ start, end, opaque_payload }`; warp
  start/end only; **output = same format as input** (preserve ASS styling, VTT
  settings, karaoke, bitmaps).
- **Tier 1 (launch):** SRT, ASS/SSA, WebVTT, MicroDVD `.sub` (frame-based → needs
  container fps). **Tier 2:** TTML/DFXP, SAMI. **Tier 3:** image subs VobSub/PGS
  (warp presentation timestamps; no OCR).
- Per-format parse↔serialize adapters; the engine is unchanged across formats.
  Evaluate the `subparse` crate vs. in-house adapters (we only need timing + a
  faithful round-trip).

## Sync engine (the heart)

One unified dynamic program, not separate alass/ffsubsync engines.

- Inputs are **interval sets**: reference activity spans (VAD on the audio, or a
  reference subtitle's cue spans) + the target subtitle's per-line spans.
- The DP assigns each line `i` an offset δ on a quantized grid, maximizing
  `Σ_i overlap(line_i + δ_i, reference) − split_penalty · #{ i : δ_i ≠ δ_{i-1} }`.
- `split_penalty = ∞` → one global δ = linear shift (**ffsubsync** case); finite
  (~7) → piecewise, absorbing ad-breaks / different cuts (**alass** case).
- An outer scan over common fps ratios (23.976 / 24 / 25 / 29.97) handles
  frame-rate drift.
- The mode is chosen by the reference source (audio-VAD → sub-to-audio;
  reference `.srt` → sub-to-sub); the DP is identical and needs no ffmpeg, so it
  stays WASM-clean.
- _Implementation note:_ clearest version first (simple overlap, straightforward
  DP). The `O(n·D)` `best_prev` precompute and prefix-sum overlap are
  optimizations to apply only if profiling shows a need.

## VAD

- alass uses `webrtc-vad` (libfvad, C); ffsubsync defaults to WebRTC VAD too —
  the proven bar.
- **Decision: `earshot`** — a pure-Rust reimplementation of the WebRTC VAD
  algorithm (no_std-capable, ~100 KiB, zero C) → alass-grade detection while the
  core stays pure-Rust / WASM-clean.
- VAD lives behind a `trait Vad`; Silero (ML/ONNX) can plug in later if accuracy
  demands. VAD only matters on the real-audio path; synthetic core tests feed
  reference spans directly.

## Licensing & ownership

- **Open source under Apache-2.0** (single license, with a `NOTICE` file) — both
  `subomatic-core` and the apps/frontends. Apache-2.0 §4(d) forces redistributors
  to reproduce the `NOTICE` credit; the license also grants patents and protects
  the "Subomatic" trademark. Permissive (not GPL) → still paid-App-Store-shippable.
- We own our code (the clean-room engine + apps). We do **not** own dependencies;
  we comply with theirs:
  - **libav** (libavcodec/libavformat/libswresample) LGPL-2.1+ → linked
    **dynamically** as a system/bundled shared library (so a user can swap it),
    built `--disable-gpl`, with its source pointed to and credited in `NOTICE`.
    This leaves our own code Apache-2.0 and is App-Store-shippable (LGPL via
    dynamic linking — the store conflict is with GPL, not LGPL).
  - **clap / wasm-bindgen / ureq / serde** and other crates (MIT/Apache/BSD) →
    attribution.
- **Clean-room rule:** implement the engine from kaegi's thesis; never copy or
  port alass's GPL source. _(Not legal advice; an IP review before a paid launch
  is still worth it.)_

## Build order & status

**Done (reviewed + CI-green):**
1. **`subomatic-core`** — cue model; **SRT / WebVTT / MicroDVD / ASS-SSA**
   parse+serialize; the unified piecewise DP (`align_offsets` + `split_penalty`),
   the fps-ratio scan (`best_alignment`), and `sync`; the `Vad` trait + a
   pure-Rust `EnergyVad`.
2. **`subomatic-cli`** (`subomatic`) — `--reference` (sub-to-sub) and `--audio`
   (audio/video → VAD → sync) modes; audio is decoded by **FFI-linking libav**,
   covering AC-3/DTS/AAC/MP3/FLAC/Opus/… in MP4/MKV/TS/OGG/WAV/….
3. **`subomatic-wasm`** + **`web/`** — wasm-bindgen bindings and a fully
   client-side browser app (the subsync.online replacement); WebAudio decodes the
   media in-page. The wasm32 build is gated in CI.
4. **`subomatic-opensubtitles`** — native OpenSubtitles REST client (search /
   login / download); request-shaping and response-parsing unit-tested.
5. A GitHub Pages **deploy workflow** (`.github/workflows/pages.yml`) for the web app.

**Remaining — achievable in-repo (optional enhancements):** an `earshot` VAD
adapter for sharper speech detection than the working `EnergyVad` (needs a 16 kHz
resampler).

**Remaining — platform-bound (needs the user's machines/accounts):**
- **Distribution builds of libav:** the decode itself is done and tested, but
  shipping it means building an LGPL (`--disable-gpl`) libav for each target
  (macOS arm64/x64, Windows arm64/x64) and bundling the dynamic libs — CI links
  the runner's *system* libav for dev binaries today.
- **Mac App Store** signing + submission; **Windows arm64/x64** signed packaging.
- Web-app: enable GitHub Pages (Settings → Pages → "GitHub Actions") and run the
  deploy workflow — the build is already automated.

(The CLI decodes AC-3/DTS and everything else today by linking libav; what's left
is per-target *packaging* of that library, not the decode itself.)
