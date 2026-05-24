// SPDX-License-Identifier: Apache-2.0
//! `subomatic` — command-line subtitle synchronizer.
//!
//! Sub-to-sub mode: re-time an out-of-sync subtitle to match a reference
//! subtitle whose timings are correct. (Audio-based sync arrives with the
//! decode layer.)

use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::Parser;
use subomatic_core::{microdvd, srt, sync, vtt, AlignParams, Format, Subtitle};

/// Re-time a subtitle to match a reference subtitle.
#[derive(Parser, Debug)]
#[command(name = "subomatic", version, about)]
struct Args {
    /// The out-of-sync subtitle to fix (.srt, .vtt, or .sub).
    input: PathBuf,

    /// A reference subtitle whose timings are correct.
    #[arg(short, long)]
    reference: PathBuf,

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

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    let input_text = std::fs::read_to_string(&args.input)?;
    let reference_text = std::fs::read_to_string(&args.reference)?;

    let format = detect_format(&args.input)?;
    let subtitle = parse(format, &input_text, args.fps)?;

    let reference = parse(detect_format(&args.reference)?, &reference_text, args.fps)?;
    let reference_spans = reference.spans();

    let params = AlignParams {
        split_penalty: args.split_penalty.unwrap_or(i64::MAX),
        ..AlignParams::default()
    };
    let synced = sync(&subtitle, &reference_spans, &params);

    let output_text = serialize(format, &synced, args.fps);
    match args.output {
        Some(path) => std::fs::write(path, output_text)?,
        // Write directly (not `print!`) so a broken pipe is a clean error, not a panic.
        None => std::io::stdout().lock().write_all(output_text.as_bytes())?,
    }
    Ok(())
}

/// Pick a subtitle format from a file extension.
fn detect_format(path: &Path) -> Result<Format, String> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("srt") => Ok(Format::SubRip),
        Some("vtt") => Ok(Format::WebVtt),
        Some("sub") => Ok(Format::MicroDvd),
        other => Err(format!(
            "unsupported subtitle extension {other:?} (expected .srt, .vtt, or .sub)"
        )),
    }
}

/// Validate a `--fps` value: must be a positive, finite number.
fn parse_fps(s: &str) -> Result<f64, String> {
    let value: f64 = s.parse().map_err(|_| format!("not a number: {s:?}"))?;
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err(format!("fps must be positive and finite, got {value}"))
    }
}

fn parse(format: Format, text: &str, fps: f64) -> Result<Subtitle, Box<dyn Error>> {
    Ok(match format {
        Format::SubRip => srt::parse(text)?,
        Format::WebVtt => vtt::parse(text)?,
        Format::MicroDvd => microdvd::parse(text, fps),
    })
}

fn serialize(format: Format, subtitle: &Subtitle, fps: f64) -> String {
    match format {
        Format::SubRip => srt::serialize(subtitle),
        Format::WebVtt => vtt::serialize(subtitle),
        Format::MicroDvd => microdvd::serialize(subtitle, fps),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_format_by_extension() {
        assert_eq!(detect_format(Path::new("a.srt")).unwrap(), Format::SubRip);
        assert_eq!(detect_format(Path::new("a.VTT")).unwrap(), Format::WebVtt);
        assert_eq!(detect_format(Path::new("a.sub")).unwrap(), Format::MicroDvd);
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
    fn parse_then_serialize_round_trips_srt() {
        let text = "1\n00:00:01,000 --> 00:00:02,000\nHi\n";
        let sub = parse(Format::SubRip, text, 25.0).unwrap();
        let out = serialize(Format::SubRip, &sub, 25.0);
        let reparsed = parse(Format::SubRip, &out, 25.0).unwrap();
        assert_eq!(reparsed.cues, sub.cues);
    }
}
