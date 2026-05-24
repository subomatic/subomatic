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
- **Decode = ffmpeg LGPL only**, via two thin adapters that hand PCM to the
  core: native FFI (ffmpeg-next / rsmpeg → libav) and web (ffmpeg.wasm). No OS
  codecs, no hand-written demuxer.
- **OpenSubtitles** fetch (REST) bolts on later.

## Decode strategy

- ffmpeg built stripped (`--disable-everything` + only the needed
  demuxers/decoders, `--disable-gpl`) and with the `truehd`/`mlp` decoders
  **disabled**.
- Sync only needs a mono ~8–16 kHz **speech envelope**, so we decode only the
  royalty-free **base/core** layer of scalable codecs (DTS core; the AC-3 sibling
  of TrueHD; the AAC-LC base of HE-AAC) and ignore the encumbered hi-fi
  extensions. Patent exposure ≈ 0.
- Runtime enumerates audio tracks and picks the first decodable one. DTS-HD
  always works (DTS core). Only TrueHD-as-sole-audio fails → graceful "open a
  GitHub issue as a feature request".
- Codecs: AAC-LC, AC-3, DTS core, E-AC-3, HE-AAC, Opus, Vorbis, FLAC, MP3, PCM.

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
  - **ffmpeg** LGPL-2.1+ → dynamically linked / user-replaceable, bundle license
    + attribution, provide/point to source, keep `--disable-gpl`.
  - **earshot** (MIT/Apache) and other crates (MIT/Apache/BSD) → attribution.
- **Clean-room rule:** implement the engine from kaegi's thesis; never copy or
  port alass's GPL source. _(Not legal advice; an IP review before a paid launch
  is still worth it.)_

## Build order

1. **`subomatic-core`** engine — **done**: cue model, SRT round-trip, the unified
   piecewise DP (`align_offsets` + `split_penalty`), the fps-ratio scan
   (`best_alignment`), and the high-level `sync`; 19 tests (shift / split /
   fps-skew / end-to-end). _Remaining:_ more Tier-1 formats (ASS/SSA, WebVTT,
   MicroDVD) and the `Vad` trait + `earshot` adapter.
2. Native decode adapter (ffmpeg-next) + a `subomatic` CLI binary; wire `earshot`
   VAD.
3. ffmpeg.wasm web adapter + browser app (to replace subsync.online).
4. Packaging: Mac App Store, Windows arm64/x64. OpenSubtitles fetch.
