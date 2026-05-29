// SPDX-License-Identifier: Apache-2.0
//! `subomatic` — command-line subtitle synchronizer and fetcher.
//!
//! - `subomatic sync <subtitle> --reference good.srt` — align to a reference.
//! - `subomatic sync <subtitle> --audio movie.mkv` — align to a track's speech
//!   (libav decodes mkv/mp4/ac-3/dts/aac/mp3/… in-process; no external tools).
//! - `subomatic fetch --query "the matrix" --languages en` — download from
//!   OpenSubtitles (needs an API key + account; via flags or env vars).

mod decode;

use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
#[cfg(feature = "earshot")]
use subomatic_core::EarshotVad;
use subomatic_core::{microdvd, sync, AlignParams, EnergyVad, Format, Span, Subtitle, Vad};
use subomatic_opensubtitles::{Client, SearchQuery};

#[derive(Parser)]
#[command(name = "subomatic", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Re-time a subtitle to a reference subtitle or an audio track.
    Sync(SyncArgs),
    /// Search OpenSubtitles and download the best-matching subtitle.
    Fetch(FetchArgs),
}

/// Which voice-activity detector to use for `--audio`.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum VadKind {
    /// Dependency-free energy-threshold detector.
    #[default]
    Energy,
    /// Sharper neural detector (the pure-Rust `earshot` crate); better on real speech.
    #[cfg(feature = "earshot")]
    Earshot,
}

#[derive(Args)]
#[command(group(ArgGroup::new("source").required(true).args(["reference", "audio"])))]
struct SyncArgs {
    /// The out-of-sync subtitle to fix (.srt, .vtt, .sub, or .ass/.ssa).
    input: PathBuf,

    /// Align to a reference subtitle whose timings are correct.
    #[arg(short, long)]
    reference: Option<PathBuf>,

    /// Align to the speech in an audio or video file (mp4/mkv/mp3/aac/flac/…).
    #[arg(short, long)]
    audio: Option<PathBuf>,

    /// Voice-activity detector to use with `--audio`.
    #[arg(long, value_enum, default_value_t = VadKind::Energy)]
    vad: VadKind,

    /// Where to write the synced subtitle (default: stdout).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Frame rate used for MicroDVD (.sub) files.
    #[arg(long, default_value_t = microdvd::DEFAULT_FPS, value_parser = parse_fps)]
    fps: f64,

    /// Allow piecewise shifts: overlap (ms) charged per offset change between
    /// consecutive cues. Omit for a single global shift.
    #[arg(long)]
    split_penalty: Option<i64>,
}

// No `Debug` derive: this struct holds credentials, which must never reach logs.
#[derive(Args)]
#[command(group(ArgGroup::new("search").required(true).args(["query", "imdb_id"])))]
struct FetchArgs {
    /// Free-text search (movie / episode title).
    #[arg(short, long)]
    query: Option<String>,

    /// Language code(s), e.g. `en` or `en,fr`.
    #[arg(short, long, default_value = "en")]
    languages: String,

    /// IMDb id (digits only, no `tt`).
    #[arg(long)]
    imdb_id: Option<String>,

    /// Where to write the subtitle (default: the result's own filename).
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// OpenSubtitles API key.
    #[arg(long, env = "OPENSUBTITLES_API_KEY")]
    api_key: String,

    /// OpenSubtitles username (required to download).
    #[arg(long, env = "OPENSUBTITLES_USERNAME")]
    username: String,

    /// OpenSubtitles password (required to download).
    #[arg(long, env = "OPENSUBTITLES_PASSWORD")]
    password: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    match Cli::parse().command {
        Command::Sync(args) => run_sync(args),
        Command::Fetch(args) => run_fetch(args),
    }
}

fn run_sync(args: SyncArgs) -> Result<(), Box<dyn Error>> {
    let format = detect_format(&args.input)?;
    let subtitle = Subtitle::parse(format, &std::fs::read_to_string(&args.input)?, args.fps)?;
    let reference_spans = sync_source_spans(&args)?;

    let params = AlignParams {
        split_penalty: args.split_penalty.unwrap_or(i64::MAX),
        ..AlignParams::default()
    };
    let synced = sync(&subtitle, &reference_spans, &params);

    let output_text = synced.serialize(args.fps);
    match args.output {
        Some(path) => std::fs::write(path, output_text)?,
        // Write directly (not `print!`) so a broken pipe is a clean error, not a panic.
        None => std::io::stdout().lock().write_all(output_text.as_bytes())?,
    }
    Ok(())
}

/// Reference activity spans from whichever source was given (the clap group
/// guarantees exactly one of `--audio` / `--reference`).
fn sync_source_spans(args: &SyncArgs) -> Result<Vec<Span>, Box<dyn Error>> {
    if let Some(audio) = &args.audio {
        let (samples, sample_rate) = decode::decode(audio)?;
        Ok(match args.vad {
            VadKind::Energy => EnergyVad::default().detect(&samples, sample_rate),
            #[cfg(feature = "earshot")]
            VadKind::Earshot => EarshotVad::default().detect(&samples, sample_rate),
        })
    } else if let Some(reference) = &args.reference {
        let text = std::fs::read_to_string(reference)?;
        Ok(Subtitle::parse(detect_format(reference)?, &text, args.fps)?.spans())
    } else {
        Err("no sync source: pass --audio or --reference".into())
    }
}

fn run_fetch(args: FetchArgs) -> Result<(), Box<dyn Error>> {
    let user_agent = concat!("subomatic/", env!("CARGO_PKG_VERSION"));
    let mut client = Client::new(args.api_key, user_agent);

    let query = SearchQuery {
        query: args.query,
        languages: Some(args.languages),
        imdb_id: args.imdb_id,
        ..Default::default()
    };
    // Pick the most-downloaded match; ties fall back to the API's own ordering.
    let best = client
        .search(&query)?
        .into_iter()
        .max_by_key(|hit| hit.download_count)
        .ok_or("no subtitles found for that search")?;

    client.login(&args.username, &args.password)?;
    let text = client.download(best.file_id)?;

    // The filename comes from the server: only ever write a sanitized basename,
    // and don't silently clobber an existing file — `--output` is the explicit
    // opt-in to choose (and overwrite) a destination.
    let output = match args.output {
        Some(path) => path,
        None => {
            let path = PathBuf::from(safe_filename(&best.file_name));
            if path.exists() {
                return Err(format!(
                    "refusing to overwrite existing file {}; pass --output to choose where to save",
                    path.display()
                )
                .into());
            }
            path
        }
    };
    std::fs::write(&output, text)?;
    eprintln!(
        "Downloaded a {} subtitle ({} downloads) -> {}",
        best.language,
        best.download_count,
        output.display()
    );
    Ok(())
}

/// Reduce a (server-provided) filename to a safe basename — no directory
/// components, no traversal, never empty.
fn safe_filename(name: &str) -> String {
    // Split on path separators and the Windows drive marker `:`, so none of
    // `../x`, `C:\x`, or `C:x` (drive-relative) can escape the directory.
    let base = name.rsplit(['/', '\\', ':']).next().unwrap_or("").trim();
    if base.is_empty() || base.chars().all(|c| c == '.') {
        "subtitle.srt".to_string()
    } else {
        base.to_string()
    }
}

/// Pick a subtitle format from a file's extension (the path→format wrapper over
/// the core [`Format::from_extension`] mapping).
fn detect_format(path: &Path) -> Result<Format, String> {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.and_then(Format::from_extension).ok_or_else(|| {
        format!("unsupported subtitle extension {ext:?} (expected .srt, .vtt, .sub, .ass, or .ssa)")
    })
}

/// Validate a `--fps` value: must be a positive, finite number.
fn parse_fps(s: &str) -> Result<f64, String> {
    let value: f64 = s.parse().map_err(|_| format!("not a number: {s:?}"))?;
    if microdvd::is_valid_fps(value) {
        Ok(value)
    } else {
        Err(microdvd::invalid_fps_message(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn detects_format_by_extension() {
        assert_eq!(detect_format(Path::new("a.srt")).unwrap(), Format::SubRip);
        assert_eq!(detect_format(Path::new("a.VTT")).unwrap(), Format::WebVtt);
        assert_eq!(detect_format(Path::new("a.sub")).unwrap(), Format::MicroDvd);
        assert_eq!(detect_format(Path::new("a.ass")).unwrap(), Format::Ass);
        assert!(detect_format(Path::new("a.txt")).is_err());
        assert!(detect_format(Path::new("noext")).is_err());
    }

    #[test]
    fn validates_fps() {
        assert!(parse_fps("25").is_ok());
        assert!(parse_fps("23.976").is_ok());
        assert!(parse_fps("0").is_err());
        assert!(parse_fps("-1").is_err());
        assert!(parse_fps("inf").is_err());
        assert!(parse_fps("abc").is_err());
    }

    #[test]
    fn safe_filename_strips_paths_and_traversal() {
        assert_eq!(safe_filename("movie.srt"), "movie.srt");
        assert_eq!(safe_filename("../../etc/passwd"), "passwd");
        assert_eq!(safe_filename("/abs/path/x.ass"), "x.ass");
        assert_eq!(safe_filename("sub\\dir\\y.vtt"), "y.vtt");
        assert_eq!(safe_filename("C:\\dir\\z.srt"), "z.srt");
        assert_eq!(safe_filename("C:movie.srt"), "movie.srt");
        assert_eq!(safe_filename(""), "subtitle.srt");
        assert_eq!(safe_filename(".."), "subtitle.srt");
        assert_eq!(safe_filename("..."), "subtitle.srt");
    }

    #[test]
    fn parse_then_serialize_round_trips_srt() {
        let text = "1\n00:00:01,000 --> 00:00:02,000\nHi\n";
        let sub = Subtitle::parse(Format::SubRip, text, 25.0).unwrap();
        let out = sub.serialize(25.0);
        let reparsed = Subtitle::parse(Format::SubRip, &out, 25.0).unwrap();
        assert_eq!(reparsed.cues, sub.cues);
    }
}
