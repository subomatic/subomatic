// SPDX-License-Identifier: Apache-2.0
//! Decode an audio/video file to mono PCM by **FFI-linking libav** (libavformat +
//! libavcodec, via the `ffmpeg-the-third` bindings).
//!
//! We link the library — we never shell out to the `ffmpeg` binary — so there is
//! no external tool to find and no CLI surface that can change underneath us. The
//! decoder normalizes whatever the file holds (any channel count, sample format,
//! and codec libav supports — AAC, AC-3, DTS, E-AC-3, MP3, FLAC, Opus, PCM, …) to
//! a single mono `f32` stream at the source rate, ready for the VAD.
//!
//! libav is LGPL-2.1+: it is linked dynamically (the default for a system libav)
//! and credited in `NOTICE`, which keeps our own Apache-2.0 code unaffected.

use std::error::Error;
use std::path::Path;

use ffmpeg::format::sample::Type as SampleType;
use ffmpeg::format::Sample;
use ffmpeg::frame::Audio as Frame;
use ffmpeg::media::Type as MediaType;
use ffmpeg::ChannelLayout;
use ffmpeg_the_third as ffmpeg;

/// Decode the audio at `path` to a mono f32 signal, with its sample rate.
pub fn decode(path: &Path) -> Result<(Vec<f32>, u32), Box<dyn Error>> {
    ffmpeg::init()?;

    let mut input = ffmpeg::format::input(&path)?;
    let stream = input
        .streams()
        .best(MediaType::Audio)
        .ok_or("no audio track in the file")?;
    let stream_index = stream.index();

    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .audio()?;

    let rate = decoder.rate();

    // Build the resampler lazily from the first decoded frame: a codec's exact
    // output spec (sample format, channel layout) isn't reliably known until it
    // has produced one. It downmixes whatever libav gives us to mono f32 at the
    // source rate (the VAD is rate-aware, so no rate conversion is needed).
    let mut resampler: Option<ffmpeg::software::resampling::Context> = None;
    let mut mono: Vec<f32> = Vec::new();
    for (stream, packet) in input.packets() {
        if stream.index() == stream_index {
            decoder.send_packet(&packet)?;
            drain(&mut decoder, &mut resampler, &mut mono)?;
        }
    }
    decoder.send_eof()?;
    drain(&mut decoder, &mut resampler, &mut mono)?;

    // With out_rate == in_rate the resampler only downmixes and reformats — both
    // stateless — so it buffers no delay and needs no flush. Assert that invariant
    // so a future change to the output rate can't silently drop tail samples
    // (swresample *can* hold a delay once it actually resamples).
    debug_assert!(
        resampler.as_ref().and_then(|r| r.delay()).is_none(),
        "resampler holds buffered samples; add a flush loop if out_rate != in_rate",
    );

    Ok((mono, rate))
}

/// Pull every decoded frame currently available, resample each to mono f32, and
/// append the samples to `mono`.
fn drain(
    decoder: &mut ffmpeg::decoder::Audio,
    resampler: &mut Option<ffmpeg::software::resampling::Context>,
    mono: &mut Vec<f32>,
) -> Result<(), ffmpeg::Error> {
    let mut decoded = Frame::empty();
    // `receive_frame` returns Err for the normal EAGAIN ("needs more input") and
    // EOF terminators; libav skips genuinely corrupt frames internally, and the
    // send_packet / send_eof / resampler calls propagate hard errors, so ending
    // the loop on the first Err is the standard, safe drain pattern.
    while decoder.receive_frame(&mut decoded).is_ok() {
        // Some decoders leave the channel layout unset; fill it with libav's
        // canonical default for that channel count (the mapping libav itself
        // assumes), so it matches the resampler's input and stays constant across
        // frames. If a later frame's spec genuinely differs, the resampler returns
        // AVERROR_INPUT_CHANGED below and we surface it as an error rather than
        // silently mixing garbage.
        if decoded.channel_layout().is_empty() {
            decoded.set_channel_layout(ChannelLayout::default(decoded.channels() as i32));
        }
        if resampler.is_none() {
            *resampler = Some(ffmpeg::software::resampling::Context::get(
                decoded.format(),
                decoded.channel_layout(),
                decoded.rate(),
                Sample::F32(SampleType::Packed),
                ChannelLayout::MONO,
                decoded.rate(),
            )?);
        }
        let resampler = resampler.as_mut().expect("resampler initialized above");
        let mut resampled = Frame::empty();
        // No sample-rate change (in == out), so the resampler buffers nothing and
        // converts each frame in full; the returned delay is always empty.
        resampler.run(&decoded, &mut resampled)?;
        mono.extend_from_slice(resampled.plane::<f32>(0));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny 16-bit mono PCM WAV with two samples at 8 kHz.
    fn tiny_wav() -> Vec<u8> {
        let data: [i16; 2] = [0, 16_384];
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&((36 + data.len() * 2) as u32).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&1u16.to_le_bytes()); // mono
        v.extend_from_slice(&8_000u32.to_le_bytes()); // sample rate
        v.extend_from_slice(&16_000u32.to_le_bytes()); // byte rate
        v.extend_from_slice(&2u16.to_le_bytes()); // block align
        v.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        v.extend_from_slice(b"data");
        v.extend_from_slice(&((data.len() * 2) as u32).to_le_bytes());
        for s in data {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    #[test]
    fn decodes_a_wav_file_via_libav() {
        let path =
            std::env::temp_dir().join(format!("subomatic_decode_{}.wav", std::process::id()));
        std::fs::write(&path, tiny_wav()).unwrap();
        let result = decode(&path);
        std::fs::remove_file(&path).ok();
        let (samples, rate) = result.unwrap();
        assert_eq!(rate, 8_000);
        assert_eq!(samples.len(), 2);
        assert!((samples[1] - 0.5).abs() < 1e-3); // 16384 / 32768
    }

    #[test]
    fn decode_errors_on_a_missing_file() {
        assert!(decode(Path::new("/no/such/file.mp3")).is_err());
    }
}
