// SPDX-License-Identifier: Apache-2.0
//! Minimal, dependency-free WAV (RIFF/PCM) decoder.
//!
//! Decodes uncompressed PCM (8/16/24/32-bit integer or 32-bit float) WAV files
//! to a mono f32 signal for voice-activity detection. Compressed audio
//! (AAC/AC-3/DTS/… inside MKV/MP4) is out of scope here — that's the job of the
//! planned ffmpeg-LGPL decode adapter.

use std::error::Error;
use std::fmt;

/// An error decoding a WAV file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WavError {
    NotRiffWave,
    Truncated,
    MissingChunk(&'static str),
    Unsupported(String),
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WavError::NotRiffWave => write!(f, "not a RIFF/WAVE file"),
            WavError::Truncated => write!(f, "truncated WAV file"),
            WavError::MissingChunk(c) => write!(f, "WAV is missing the {c:?} chunk"),
            WavError::Unsupported(s) => write!(f, "unsupported WAV format ({s})"),
        }
    }
}

impl Error for WavError {}

/// Decoded mono PCM samples plus their sample rate.
#[derive(Clone, Debug)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Decode a WAV file into mono f32 PCM.
pub fn decode(bytes: &[u8]) -> Result<Pcm, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotRiffWave);
    }

    // Walk the chunk list, capturing `fmt ` and `data`.
    let mut fmt_chunk: Option<&[u8]> = None;
    let mut data: Option<&[u8]> = None;
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let start = pos + 8;
        let end = start.checked_add(size).ok_or(WavError::Truncated)?;
        if end > bytes.len() {
            return Err(WavError::Truncated);
        }
        match id {
            b"fmt " => fmt_chunk = Some(&bytes[start..end]),
            b"data" => data = Some(&bytes[start..end]),
            _ => {}
        }
        // Chunks are word-aligned: an odd size is followed by a pad byte.
        pos = end + (size & 1);
    }

    let fmt = fmt_chunk.ok_or(WavError::MissingChunk("fmt "))?;
    let data = data.ok_or(WavError::MissingChunk("data"))?;
    if fmt.len() < 16 {
        return Err(WavError::Truncated);
    }

    let audio_format = u16::from_le_bytes([fmt[0], fmt[1]]);
    let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
    if channels == 0 {
        return Err(WavError::Unsupported("0 channels".to_string()));
    }
    let channels = channels as usize;
    let sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
    let bits = u16::from_le_bytes([fmt[14], fmt[15]]);

    let interleaved = decode_samples(data, audio_format, bits)?;
    if !interleaved.len().is_multiple_of(channels) {
        return Err(WavError::Truncated); // not a whole number of frames
    }
    Ok(Pcm {
        samples: downmix(&interleaved, channels),
        sample_rate,
    })
}

/// Convert raw sample bytes to interleaved f32 in `[-1.0, 1.0]`. Rejects data
/// that isn't a whole number of samples.
fn decode_samples(data: &[u8], format: u16, bits: u16) -> Result<Vec<f32>, WavError> {
    // format 1 = integer PCM, 3 = IEEE float.
    let bytes_per_sample = match (format, bits) {
        (1, 8) => 1,
        (1, 16) => 2,
        (1, 24) => 3,
        (1, 32) | (3, 32) => 4,
        _ => {
            return Err(WavError::Unsupported(format!(
                "format {format}, {bits}-bit"
            )))
        }
    };
    if !data.len().is_multiple_of(bytes_per_sample) {
        return Err(WavError::Truncated);
    }

    let samples = match (format, bits) {
        (1, 8) => data
            .iter()
            .map(|&b| (f32::from(b) - 128.0) / 128.0)
            .collect(),
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32_768.0)
            .collect(),
        (1, 24) => data
            .chunks_exact(3)
            .map(|c| {
                let raw = i32::from(c[0]) | (i32::from(c[1]) << 8) | (i32::from(c[2]) << 16);
                let signed = (raw << 8) >> 8; // sign-extend 24 -> 32 bits
                signed as f32 / 8_388_608.0
            })
            .collect(),
        (1, 32) => data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            .collect(),
        // (3, 32): IEEE float.
        _ => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    };
    Ok(samples)
}

/// Average interleaved channels down to a mono signal. `interleaved.len()` is a
/// whole number of `channels`-sized frames (checked by the caller).
fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_wav(channels: u16, sample_rate: u32, bits: u16, format: u16, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((4 + 24 + 8 + data.len()) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&format.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * u32::from(channels) * u32::from(bits / 8);
        v.extend_from_slice(&byte_rate.to_le_bytes());
        v.extend_from_slice(&(channels * (bits / 8)).to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn decodes_16bit_mono() {
        let mut data = Vec::new();
        data.extend_from_slice(&0i16.to_le_bytes());
        data.extend_from_slice(&16_384i16.to_le_bytes()); // = 0.5
        let pcm = decode(&make_wav(1, 8_000, 16, 1, &data)).unwrap();
        assert_eq!(pcm.sample_rate, 8_000);
        assert_eq!(pcm.samples.len(), 2);
        assert!(pcm.samples[0].abs() < 1e-6);
        assert!((pcm.samples[1] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn downmixes_stereo_to_mono() {
        let mut data = Vec::new();
        data.extend_from_slice(&16_384i16.to_le_bytes()); // L = 0.5
        data.extend_from_slice(&(-16_384i16).to_le_bytes()); // R = -0.5
        let pcm = decode(&make_wav(2, 8_000, 16, 1, &data)).unwrap();
        assert_eq!(pcm.samples.len(), 1);
        assert!(pcm.samples[0].abs() < 1e-3); // averages to ~0
    }

    #[test]
    fn rejects_non_wav() {
        assert_eq!(
            decode(b"not a wav file!!").unwrap_err(),
            WavError::NotRiffWave
        );
    }

    #[test]
    fn rejects_truncated_sample_data() {
        // 16-bit PCM but an odd number of data bytes.
        let wav = make_wav(1, 8_000, 16, 1, [0u8, 0u8, 0u8].as_slice());
        assert_eq!(decode(&wav).unwrap_err(), WavError::Truncated);
    }

    #[test]
    fn rejects_zero_channels() {
        let wav = make_wav(0, 8_000, 16, 1, [0u8, 0u8].as_slice());
        assert!(matches!(
            decode(&wav).unwrap_err(),
            WavError::Unsupported(_)
        ));
    }
}
