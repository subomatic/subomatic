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

- **Windows.** Since June 2023 a publicly-trusted code-signing key must live on
  FIPS-140 hardware (an HSM / USB token or a cloud HSM), which shapes the options:
  - **Microsoft Store (easiest).** Submit an MSIX and the Store *signs it for you*
    — no certificate of your own and no SmartScreen "unknown publisher" warning.
    Needs a Partner Center account (~$19 one-time individual / ~$99 company). Best
    paired with the Tauri/MSIX wrapper below; this is the Mac App Store analogue.
  - **Azure Trusted Signing (best for CI / direct downloads).** Microsoft's
    managed cloud HSM, ~$10/month, signs straight from CI. `release.yml` already
    has an *optional, secrets-gated* step (`azure/trusted-signing-action`) that
    runs only when all six of these repository secrets are set: `AZURE_TENANT_ID`,
    `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TRUSTED_SIGNING_ENDPOINT`,
    `AZURE_TRUSTED_SIGNING_ACCOUNT`, `AZURE_TRUSTED_SIGNING_PROFILE`. Note: the
    *individual* tier is US/Canada-only today; in the EU it's open to
    *organizations* (so an EU individual would enroll as an org).
  - **Traditional CA cert + token (DigiCert / Sectigo / SSL.com).** An OV
    (~$200–400/yr; SmartScreen trust accrues over time) or EV (~$300–700/yr;
    instant SmartScreen trust) certificate on a token. Sign with
    `signtool sign /fd SHA256 /tr <timestamp-url> /td SHA256 subomatic.exe` (the
    timestamp keeps signatures valid past cert expiry); swap this in for the Azure
    step if you go this route. arm64 and x64 sign identically.

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

- A `[workspace.lints]` table to enforce the zero-warnings policy in local builds,
  not just CI.

(The sharper `earshot` voice-activity detector is already implemented — build the
CLI and run `subomatic sync --audio <file> --vad earshot`.)
