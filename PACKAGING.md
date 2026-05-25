# Packaging & remaining platform work

Everything in this repo is built, tested, reviewed, and CI-green. The steps below
are the remaining phases — each needs a machine, account, or certificate that
can't live in CI/source, so they're written as a checklist for the maintainer.

## 1. Deploy the web app (≈1 minute)

1. Repo **Settings → Pages → Source: "GitHub Actions"**.
2. **Actions → "Deploy web app to GitHub Pages" → Run workflow.**

It builds the WASM bundle and publishes `web/` to
`https://lgibelli.github.io/subomatic`. (The workflow is `.github/workflows/pages.yml`.)

## 2. Bundle an LGPL libav for distribution

Audio decode is **done and in-repo**: the CLI FFI-links libav (libavcodec /
libavformat / libswresample, via `ffmpeg-the-third`) and decodes every codec libav
supports — AC-3, DTS, E-AC-3, AAC, MP3, FLAC, Opus, Vorbis, ALAC, PCM, … inside
MP4/MKV/TS/OGG/WAV/… (verified end-to-end on AC-3 and DTS). The **web** app decodes
via WebAudio. What remains is *packaging* that library for shipping:

1. **Build libav LGPL, decode-only** for each target (macOS arm64/x64, Windows
   arm64/x64): `--disable-gpl` (no GPL codecs/filters), ideally
   `--disable-everything` plus only the demuxers/decoders you ship, and
   `--disable-decoder=truehd,mlp` for patent hygiene. This keeps the result
   **LGPL** — Apache-app- and App-Store-compatible — not GPL.
2. **Link dynamically and bundle the shared libs** inside the `.app`/MSIX
   (`@rpath`/`@loader_path` on macOS; the DLLs beside the `.exe` on Windows). The
   user must be able to swap the library — dynamic linking gives that for free and
   satisfies LGPL §6.
3. **Carry the obligations:** ship libav's LGPL text + attribution in `NOTICE`,
   and make the exact libav source you shipped available.
4. **Build-time tooling:** each target needs libav dev libs + libclang (bindgen
   generates the FFI). CI installs these from the system package manager for *dev*
   binaries (see `release.yml`); a distribution build should point at your own
   `--disable-gpl` libav (e.g. via `FFMPEG_DIR`).

Patent note: we only extract a speech envelope, AC-3's core patents have largely
expired, and we keep `--disable-gpl`; an IP review before a *paid* launch is still
prudent (same caveat as the clean-room engine).

## 3. Desktop apps — Mac App Store & Windows

**Unsigned CLI binaries are already automated:** push a tag `vX.Y.Z` and
`.github/workflows/release.yml` builds macOS (arm64/x64), Windows (x64), and
Linux (x64) binaries. Then sign for distribution:

- **macOS:** `codesign` + notarize (`notarytool`) with a Developer ID; for the
  App Store, wrap the binary (or the Tauri app below) in a bundle and submit via
  App Store Connect. Requires a paid Apple Developer account.
- **Windows:** sign the `.exe`/MSIX with a code-signing certificate (`signtool`).

**For a GUI app (what the stores expect),** wrap the existing `web/` front-end in
**Tauri** (reuses `subomatic-wasm`): `cargo create-tauri-app`, point it at `web/`,
then `cargo tauri build` per platform, then sign/submit as above. I can scaffold
`src-tauri/` on request — the build/sign/submit still happen on your machines.

## 4. OpenSubtitles (live use)

`subomatic fetch` is wired and tested offline. Register an app at
opensubtitles.com for an API key, then:

```sh
export OPENSUBTITLES_API_KEY=… OPENSUBTITLES_USERNAME=… OPENSUBTITLES_PASSWORD=…
subomatic fetch --query "the matrix" --languages en
```

## Optional polish (in-repo, no external resources)

- An `earshot` (WebRTC-VAD) implementation of `subomatic_core::Vad` for sharper
  speech detection than `EnergyVad` (needs a 16 kHz resampler).
- A `[workspace.lints]` table to enforce the zero-warnings policy in local builds,
  not just CI.
