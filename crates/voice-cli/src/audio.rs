//! Fixed-length microphone recording and WAV playback on the default devices.
use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    path::Path,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};
use voice_core::audio::{
    resample::{f32_to_i16, i16_to_f32, Resampler},
    wav::{bytes_to_samples, samples_to_bytes, wrap_pcm16le},
};

/// Parsed 16-bit PCM WAV audio.
#[derive(Debug, PartialEq)]
pub struct WavAudio {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// Read a 16-bit PCM WAV, walking its chunks to find `fmt ` and `data`.
pub fn parse_wav(bytes: &[u8]) -> anyhow::Result<WavAudio> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(anyhow!("not a WAV file"));
    }
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let (mut pos, mut format) = (12usize, None);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(pos + 4) as usize;
        let body = pos + 8;
        // Streamed WAVs may declare a size larger than the bytes that follow.
        let end = body.saturating_add(size).min(bytes.len());
        if id == b"fmt " && end >= body + 16 {
            format = Some((
                u16_at(body),
                u16_at(body + 2),
                u32_at(body + 4),
                u16_at(body + 14),
            ));
        } else if id == b"data" {
            let (kind, channels, sample_rate, bits) =
                format.ok_or_else(|| anyhow!("WAV has no format chunk"))?;
            if kind != 1 || bits != 16 {
                return Err(anyhow!(
                    "only 16-bit PCM WAV is supported (format {kind}, {bits} bits)"
                ));
            }
            return Ok(WavAudio {
                sample_rate,
                channels,
                samples: bytes_to_samples(&bytes[body..end]),
            });
        }
        pos = body + size + (size & 1);
    }
    Err(anyhow!("WAV has no audio data"))
}

/// Record `seconds` of mono audio from the default microphone at `sample_rate`.
pub fn record_wav(output_path: &Path, sample_rate: u32, seconds: u32) -> anyhow::Result<()> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or_else(|| anyhow!("no default microphone"))?;
    let supported = device
        .default_input_config()
        .context("microphone has no usable format")?;
    let config: cpal::StreamConfig = supported.config();
    let channels = usize::from(config.channels).max(1);
    let wanted = (sample_rate * seconds) as usize;

    let state = Arc::new(Mutex::new((
        Resampler::new(config.sample_rate, sample_rate),
        Vec::<f32>::new(),
    )));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let push = {
        let state = state.clone();
        move |interleaved: &mut dyn Iterator<Item = f32>| {
            let mut mono = Vec::new();
            let mut frame = Vec::with_capacity(channels);
            for sample in interleaved {
                frame.push(sample);
                if frame.len() == channels {
                    mono.push(frame.iter().sum::<f32>() / channels as f32);
                    frame.clear();
                }
            }
            let mut guard = state.lock().unwrap();
            let (resampler, out) = &mut *guard;
            if out.len() < wanted {
                resampler.process(&mono, out);
                if out.len() >= wanted {
                    let _ = done_tx.send(());
                }
            }
        }
    };
    let on_error = |error| eprintln!("microphone error: {error}");
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config,
            move |d: &[f32], _: &_| push(&mut d.iter().copied()),
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |d: &[i16], _: &_| push(&mut d.iter().map(|s| i16_to_f32(*s))),
            on_error,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |d: &[u16], _: &_| {
                push(&mut d.iter().map(|s| (f32::from(*s) - 32768.0) / 32768.0))
            },
            on_error,
            None,
        )?,
        other => return Err(anyhow!("unsupported microphone sample format {other:?}")),
    };
    stream.play()?;
    done_rx
        .recv_timeout(Duration::from_secs(u64::from(seconds) + 10))
        .map_err(|_| anyhow!("the microphone delivered no audio"))?;
    drop(stream);

    let mut recorded = std::mem::take(&mut state.lock().unwrap().1);
    recorded.truncate(wanted);
    let samples: Vec<i16> = recorded.into_iter().map(f32_to_i16).collect();
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        output_path,
        wrap_pcm16le(&samples_to_bytes(&samples), sample_rate, 1),
    )?;
    Ok(())
}

/// Play a 16-bit PCM WAV on the default speakers and wait until it finishes.
pub fn play_wav(path: &Path) -> anyhow::Result<()> {
    let wav = parse_wav(&std::fs::read(path)?)?;
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| anyhow!("no default speaker"))?;
    let supported = device
        .default_output_config()
        .context("speaker has no usable format")?;
    let config: cpal::StreamConfig = supported.config();
    let out_channels = usize::from(config.channels).max(1);

    let in_channels = usize::from(wav.channels).max(1);
    let mono: Vec<f32> = wav
        .samples
        .chunks(in_channels)
        .map(|frame| frame.iter().map(|s| i16_to_f32(*s)).sum::<f32>() / in_channels as f32)
        .collect();
    let mut resampled = Vec::with_capacity(mono.len() * 2);
    Resampler::new(wav.sample_rate, config.sample_rate).process(&mono, &mut resampled);
    let seconds = resampled.len() as f64 / f64::from(config.sample_rate);

    let queue = Arc::new(Mutex::new(resampled.into_iter()));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    let next = {
        let queue = queue.clone();
        move || match queue.lock().unwrap().next() {
            Some(sample) => sample,
            None => {
                let _ = done_tx.send(());
                0.0
            }
        }
    };
    let on_error = |error| eprintln!("speaker error: {error}");
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &_| {
                data.chunks_mut(out_channels).for_each(|f| f.fill(next()))
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &_| {
                data.chunks_mut(out_channels)
                    .for_each(|f| f.fill(f32_to_i16(next())))
            },
            on_error,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _: &_| {
                data.chunks_mut(out_channels)
                    .for_each(|f| f.fill(((next().clamp(-1.0, 1.0) * 32767.0) + 32768.0) as u16))
            },
            on_error,
            None,
        )?,
        other => return Err(anyhow!("unsupported speaker sample format {other:?}")),
    };
    stream.play()?;
    let _ = done_rx.recv_timeout(Duration::from_secs_f64(seconds + 5.0));
    // Give the device a moment to play out its last buffer.
    std::thread::sleep(Duration::from_millis(250));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_canonical_wav() {
        let wav = wrap_pcm16le(&samples_to_bytes(&[1, -2, 3]), 24000, 1);
        assert_eq!(
            parse_wav(&wav).unwrap(),
            WavAudio {
                sample_rate: 24000,
                channels: 1,
                samples: vec![1, -2, 3]
            }
        );
    }

    #[test]
    fn skips_extra_chunks_and_tolerates_streamed_sizes() {
        let mut wav = wrap_pcm16le(&samples_to_bytes(&[7, 8]), 8000, 1);
        // Insert a LIST chunk between fmt and data, and declare an oversized data length.
        let data_at = 36;
        let mut list = b"LIST".to_vec();
        list.extend_from_slice(&3u32.to_le_bytes());
        list.extend_from_slice(b"abc\0");
        wav.splice(data_at..data_at, list);
        let size_at = data_at + 12 + 4;
        wav[size_at..size_at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        assert_eq!(parse_wav(&wav).unwrap().samples, vec![7, 8]);
    }

    #[test]
    fn rejects_non_wav_and_non_pcm() {
        assert!(parse_wav(b"not a wav file at all").is_err());
        let mut wav = wrap_pcm16le(&[0, 0], 8000, 1);
        wav[20] = 3; // IEEE float
        assert!(parse_wav(&wav).is_err());
    }
}
