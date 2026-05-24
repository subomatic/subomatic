# Subomatic

Fully-automatic subtitle synchronization — re-time an out-of-sync subtitle to a
video/audio track's speech, or to a reference subtitle. No manual anchor points.
One shared Rust core targets a CLI, the browser (WASM), and (later) desktop apps.

## What works today

- **Engine** — a unified alignment dynamic program: a single global shift,
  piecewise shifts that absorb ad-breaks / different cuts (a `split_penalty`
  knob), and a frame-rate-drift scan (23.976↔25, …).
- **Formats** — SubRip (`.srt`), WebVTT (`.vtt`), MicroDVD (`.sub`), and
  Advanced SubStation Alpha (`.ass`/`.ssa`); timing-only, so styling and text
  round-trip untouched.
- **CLI** (`subomatic`) — sync to a reference subtitle or to a WAV track's speech.
- **Web** — a browser app over the WASM core; WebAudio decodes the media in-page,
  so any format your browser can play drives the sync. Nothing is uploaded.

## Workspace

- `crates/subomatic-core` — pure Rust, `#![forbid(unsafe_code)]`, native + WASM:
  the subtitle model, format adapters, voice-activity detection, and the engine.
- `crates/subomatic-cli` — the `subomatic` command-line tool.
- `crates/subomatic-wasm` — wasm-bindgen bindings for the browser.
- `web/` — the browser front-end (static HTML/JS over the WASM bindings).

## CLI

```sh
# Re-time a subtitle:
cargo run -p subomatic-cli -- sync late.srt --reference good.srt -o fixed.srt
cargo run -p subomatic-cli -- sync late.srt --audio movie.wav -o fixed.srt

# Fetch from OpenSubtitles (API key + account via flags or env vars):
cargo run -p subomatic-cli -- fetch --query "the matrix" --languages en \
  --api-key "$OPENSUBTITLES_API_KEY"
```

`sync --reference` aligns to a known-good subtitle; `sync --audio` aligns to a
WAV track's speech (`--split-penalty <ms>` enables piecewise shifts, `--fps`
sets the MicroDVD rate). `fetch` searches OpenSubtitles and downloads the
most-downloaded match.

## Web app

```sh
# Build the WASM bundle into web/pkg, then serve web/.
wasm-pack build crates/subomatic-wasm --target web --out-dir ../../web/pkg
python3 -m http.server --directory web 8080   # then open http://localhost:8080
```

## Build & test

```sh
cargo test
```

CI also runs `cargo fmt --check`, `cargo clippy -- -D warnings`, and a
`wasm32-unknown-unknown` build.

## License

Subomatic is licensed under the **Apache License, Version 2.0** — see
[`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Reuse must preserve the `NOTICE`
attribution (Apache-2.0 §4(d)).

### Third-party

- **clap** (CLI) and **wasm-bindgen** (web): MIT OR Apache-2.0.
- **ffmpeg** (planned, for compressed-audio decode in the native CLI): LGPL-2.1+,
  to be linked dynamically and user-replaceable.

The synchronization algorithm is a **clean-room** implementation derived from
kaegi's published thesis (*"Automatic Language-Agnostic Subtitle
Synchronization"*); no GPL `alass` source is used, so Subomatic stays
App-Store-shippable.

See [`DESIGN.md`](DESIGN.md) for the architecture, decisions, and remaining work.
