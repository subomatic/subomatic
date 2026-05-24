# Packaging & remaining platform work

Everything in this repo is built, tested, reviewed, and CI-green. The steps below
are the remaining phases — each needs a machine, account, or certificate that
can't live in CI/source, so they're written as a checklist for the maintainer.

## 1. Deploy the web app (≈1 minute)

1. Repo **Settings → Pages → Source: "GitHub Actions"**.
2. **Actions → "Deploy web app to GitHub Pages" → Run workflow.**

It builds the WASM bundle and publishes `web/` to
`https://lgibelli.github.io/subomatic`. (The workflow is `.github/workflows/pages.yml`.)

## 2. Compressed-audio decode in the native CLI (ffmpeg-LGPL)

The **web** app already syncs to compressed audio (WebAudio decodes it in-page).
The **CLI** currently decodes WAV only. To extend it to MKV/MP4/AC-3/DTS/…:

1. Add a `crates/subomatic-decode` crate depending on `ffmpeg-next`
   (libavformat + libavcodec).
2. Build ffmpeg **LGPL, decode-only**: `--disable-gpl --disable-decoder=truehd,mlp`
   and only the needed demuxers/decoders (see the codec strategy in `DESIGN.md`).
3. Decode the first decodable audio track to **mono ~16 kHz f32**, then feed
   `subomatic_core::EnergyVad` (the same spans the WAV path produces today).
4. Branch `subomatic sync --audio <file>` to use it when the input isn't WAV.

Needs ffmpeg dev libraries locally, and the per-target builds for distribution.
(I can write this crate against your ffmpeg install — it just can't compile in
this sandbox, which has no ffmpeg libs.)

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
