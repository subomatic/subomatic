# Subomatic

Fully-automatic subtitle synchronization — drop in a video (or its audio) and a
subtitle, and Subomatic re-times the subtitle to match. No manual anchor points.
Targets the web (WASM), the Mac App Store, and Windows (arm64/x64) from one
shared Rust core.

> **Status:** early scaffold. The timing-only core (`subomatic-core`) is being
> built and tested first; the ffmpeg decode layer, the frontends, and
> OpenSubtitles fetch come later. See [`DESIGN.md`](DESIGN.md) for the full
> architecture and the decisions behind it.

## Workspace

- **`crates/subomatic-core`** — pure Rust, `#![forbid(unsafe_code)]`: the
  subtitle model, format adapters (SRT first), and the alignment engine.
  Compiles to native and `wasm32-unknown-unknown`.

## Build & test

```sh
cargo test
```

## License

Subomatic is licensed under the **Apache License, Version 2.0** — see
[`LICENSE`](LICENSE) and [`NOTICE`](NOTICE). Anyone who reuses the code must
preserve the `NOTICE` attribution (Apache-2.0 §4(d)).

### Third-party

- **ffmpeg** (decode layer, added later): LGPL-2.1+, linked dynamically and
  user-replaceable; its license and source are bundled/linked per the LGPL.
- **earshot** (voice-activity detection, added later): MIT OR Apache-2.0.

The synchronization algorithm is a **clean-room** implementation derived from
kaegi's published thesis (*"Automatic Language-Agnostic Subtitle
Synchronization"*); no GPL `alass` source is used, so Subomatic stays
App-Store-shippable.
