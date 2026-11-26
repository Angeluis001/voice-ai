//! Local mode: talk to the agent through this PC's microphone and speakers.
//!
//! The microphone is downmixed to mono, resampled to the session rate and sent to the
//! call engine in 20 ms frames. AI audio is resampled to the speaker's rate and played
//! from a queue; a barge-in simply empties that queue.
use anyhow::{anyhow, Context};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::mpsc;
use voice_core::{
    audio::{
        resample::{f32_to_i16, i16_to_f32, Resampler},
        wav::bytes_to_samples,
    },
    call::{run_call, CallDeps, CallOutcome, CallParams, LegCommand, LegEvent},
};

#[derive(Debug, Clone, Default)]
pub struct TalkOptions {
    pub client_id: Option<String>,
    pub roleplay: bool,
    pub scenario: String,
    /// Mute the microphone while the AI is speaking (for speakers without a headset).
    pub half_duplex: bool,
    /// Play these WAV files as the caller instead of using the microphone, one
    /// utterance per file, each after the AI has finished speaking.
    pub caller_wavs: Vec<std::path::PathBuf>,
    /// Hang up automatically after this many seconds (for unattended checks).
    pub max_seconds: Option<u64>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "unknown device".into())
}

fn pick_device(devices: impl Iterator<Item = cpal::Device>, wanted: &str) -> Option<cpal::Device> {
    let wanted = wanted.to_lowercase();
    devices
        .into_iter()
        .find(|d| device_name(d).to_lowercase().contains(&wanted))
}

/// Print the audio devices this PC offers.
pub fn list_devices() -> anyhow::Result<()> {
    let host = cpal::default_host();
    let default_in = host
        .default_input_device()
        .map(|d| device_name(&d))
        .unwrap_or_default();
    let default_out = host
        .default_output_device()
        .map(|d| device_name(&d))
        .unwrap_or_default();
    println!("Microphones (set VOICE_AI_INPUT_DEVICE to part of a name):");
    for device in host.input_devices()? {
        let name = device_name(&device);
        println!(
            "  {}{}",
            name,
            if name == default_in {
                "   <- default"
            } else {
                ""
            }
        );
    }
    println!("Speakers (set VOICE_AI_OUTPUT_DEVICE to part of a name):");
    for device in host.output_devices()? {
        let name = device_name(&device);
        println!(
            "  {}{}",
            name,
            if name == default_out {
                "   <- default"
            } else {
                ""
            }
        );
    }
    Ok(())
}

/// State shared between the audio callbacks and the call.
struct Shared {
    /// AI audio waiting to be played, at the speaker's sample rate.
    playout: Mutex<VecDeque<f32>>,
    /// Last time the speaker actually played AI audio (ms since epoch).
    last_playback_ms: AtomicU64,
    stopping: AtomicBool,
}

/// Turns device-rate, multi-channel input into mono PCM16 frames at the session rate.
struct MicFramer {
    channels: usize,
    resampler: Resampler,
    pending: Vec<f32>,
    frame_samples: usize,
    half_duplex: bool,
    shared: Arc<Shared>,
    tx: mpsc::Sender<LegEvent>,
}

impl MicFramer {
    fn push(&mut self, interleaved: impl Iterator<Item = f32>) {
        let mut mono = Vec::new();
        let mut acc = 0.0f32;
        let mut n = 0usize;
        for sample in interleaved {
            acc += sample;
            n += 1;
            if n == self.channels {
                mono.push(acc / self.channels as f32);
                acc = 0.0;
                n = 0;
            }
        }
        self.resampler.process(&mono, &mut self.pending);
        // With open speakers the mic would hear the AI and interrupt it.
        let muted = self.half_duplex
            && now_ms().saturating_sub(self.shared.last_playback_ms.load(Ordering::Relaxed)) < 350;
        while self.pending.len() >= self.frame_samples {
            let frame: Vec<u8> = self
                .pending
                .drain(..self.frame_samples)
                .flat_map(|s| f32_to_i16(if muted { 0.0 } else { s }).to_le_bytes())
                .collect();
            // If the engine is busy, dropping a frame is better than blocking the audio thread.
            let _ = self.tx.try_send(LegEvent::Audio(frame));
        }
    }
}

fn build_input(
    device: &cpal::Device,
    session_rate: u32,
    half_duplex: bool,
    shared: Arc<Shared>,
    tx: mpsc::Sender<LegEvent>,
) -> anyhow::Result<cpal::Stream> {
    let supported = device
        .default_input_config()
        .context("microphone has no usable format")?;
    let config: cpal::StreamConfig = supported.config();
    let device_rate = config.sample_rate;
    let mut framer = MicFramer {
        channels: usize::from(config.channels).max(1),
        resampler: Resampler::new(device_rate, session_rate),
        pending: Vec::new(),
        frame_samples: (session_rate / 50) as usize,
        half_duplex,
        shared,
        tx: tx.clone(),
    };
    let on_error = move |error| {
        let _ = tx.try_send(LegEvent::Error(format!("microphone: {error}")));
    };
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &_| framer.push(data.iter().copied()),
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &_| framer.push(data.iter().map(|s| i16_to_f32(*s))),
            on_error,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &_| {
                framer.push(data.iter().map(|s| (f32::from(*s) - 32768.0) / 32768.0))
            },
            on_error,
            None,
        )?,
        other => return Err(anyhow!("unsupported microphone sample format {other:?}")),
    };
    Ok(stream)
}

fn fill_output<T: Copy>(
    data: &mut [T],
    channels: usize,
    shared: &Shared,
    convert: impl Fn(f32) -> T,
) {
    let mut queue = shared.playout.lock().unwrap();
    let mut played = false;
    for frame in data.chunks_mut(channels) {
        let sample = match queue.pop_front() {
            Some(s) => {
                played = true;
                s
            }
            None => 0.0,
        };
        frame.fill(convert(sample));
    }
    if played {
        shared.last_playback_ms.store(now_ms(), Ordering::Relaxed);
    }
}

fn build_output(device: &cpal::Device, shared: Arc<Shared>) -> anyhow::Result<(cpal::Stream, u32)> {
    let supported = device
        .default_output_config()
        .context("speaker has no usable format")?;
    let config: cpal::StreamConfig = supported.config();
    let channels = usize::from(config.channels).max(1);
    let device_rate = config.sample_rate;
    let on_error = |error| tracing::warn!(phase = "speaker_error", %error);
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |data: &mut [f32], _: &_| fill_output(data, channels, &shared, |s| s),
            on_error,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |data: &mut [i16], _: &_| fill_output(data, channels, &shared, f32_to_i16),
            on_error,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_output_stream(
            config,
            move |data: &mut [u16], _: &_| {
                fill_output(data, channels, &shared, |s| {
                    ((s.clamp(-1.0, 1.0) * 32767.0) + 32768.0) as u16
                })
            },
            on_error,
            None,
        )?,
        other => return Err(anyhow!("unsupported speaker sample format {other:?}")),
    };
    Ok((stream, device_rate))
}

/// Load caller utterances as mono samples at the session rate.
fn load_utterances(
    paths: &[std::path::PathBuf],
    session_rate: u32,
) -> anyhow::Result<Vec<Vec<i16>>> {
    paths
        .iter()
        .map(|path| {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let (rate, channels, samples) = voice_core::audio::wav::read_pcm16(&bytes)
                .map_err(|e| anyhow!("{}: {e}", path.display()))?;
            let channels = usize::from(channels).max(1);
            let mono: Vec<f32> = samples
                .chunks(channels)
                .map(|frame| frame.iter().map(|s| i16_to_f32(*s)).sum::<f32>() / channels as f32)
                .collect();
            let mut out = Vec::new();
            Resampler::new(rate, session_rate).process(&mono, &mut out);
            Ok(out.into_iter().map(f32_to_i16).collect())
        })
        .collect()
}

/// Stand in for the microphone: a steady stream of 20 ms frames that is silent
/// except when an utterance is spoken. Each utterance starts once the AI has been
/// quiet for a moment, like a person waiting for their turn.
async fn scripted_caller(
    utterances: Vec<Vec<i16>>,
    session_rate: u32,
    shared: Arc<Shared>,
    tx: mpsc::Sender<LegEvent>,
) {
    let frame_samples = (session_rate / 50) as usize;
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut queue = utterances.into_iter();
    let mut speaking: Option<(Vec<i16>, usize)> = None;
    let mut ai_spoke_since_last = false;
    let mut last_finished = now_ms();
    loop {
        ticker.tick().await;
        let now = now_ms();
        let last_playback = shared.last_playback_ms.load(Ordering::Relaxed);
        let ai_quiet =
            now.saturating_sub(last_playback) > 1200 && shared.playout.lock().unwrap().is_empty();
        if speaking.is_none() && last_playback > last_finished {
            ai_spoke_since_last = true;
        }
        let waited_long = now.saturating_sub(last_finished) > 12_000;
        if speaking.is_none() && ai_quiet && (ai_spoke_since_last || waited_long) {
            if let Some(next) = queue.next() {
                println!(
                    "[caller] speaking ({:.1} s)",
                    next.len() as f64 / f64::from(session_rate)
                );
                speaking = Some((next, 0));
                ai_spoke_since_last = false;
            }
        }
        let mut frame = vec![0i16; frame_samples];
        if let Some((samples, position)) = &mut speaking {
            let end = (*position + frame_samples).min(samples.len());
            frame[..end - *position].copy_from_slice(&samples[*position..end]);
            *position = end;
            if end == samples.len() {
                speaking = None;
                last_finished = now_ms();
                ai_spoke_since_last = false;
            }
        }
        let bytes: Vec<u8> = frame.iter().flat_map(|s| s.to_le_bytes()).collect();
        if tx.send(LegEvent::Audio(bytes)).await.is_err() {
            break;
        }
    }
}

/// Hold one conversation through the microphone and speakers. Ends on Ctrl+C,
/// when the AI hands the caller to a person, or when xAI closes the session.
pub async fn run(deps: CallDeps, options: TalkOptions) -> anyhow::Result<CallOutcome> {
    let settings = deps.settings.clone();
    let session_rate = settings.local_sample_rate;
    let host = cpal::default_host();
    let input = match &settings.input_device {
        Some(name) => pick_device(host.input_devices()?, name)
            .ok_or_else(|| anyhow!("no microphone matching \"{name}\""))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default microphone"))?,
    };
    let output = match &settings.output_device {
        Some(name) => pick_device(host.output_devices()?, name)
            .ok_or_else(|| anyhow!("no speaker matching \"{name}\""))?,
        None => host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default speaker"))?,
    };

    let shared = Arc::new(Shared {
        playout: Mutex::new(VecDeque::new()),
        last_playback_ms: AtomicU64::new(0),
        stopping: AtomicBool::new(false),
    });
    let (event_tx, event_rx) = mpsc::channel::<LegEvent>(4096);
    let (command_tx, mut command_rx) = mpsc::channel::<LegCommand>(4096);

    let scripted = !options.caller_wavs.is_empty();
    let in_stream = if scripted {
        None
    } else {
        let stream = build_input(
            &input,
            session_rate,
            options.half_duplex,
            shared.clone(),
            event_tx.clone(),
        )?;
        stream.play()?;
        Some(stream)
    };
    let (out_stream, speaker_rate) = build_output(&output, shared.clone())?;
    out_stream.play()?;
    let caller = match scripted {
        true => {
            let utterances = load_utterances(&options.caller_wavs, session_rate)?;
            Some(tokio::spawn(scripted_caller(
                utterances,
                session_rate,
                shared.clone(),
                event_tx.clone(),
            )))
        }
        false => None,
    };

    if scripted {
        println!(
            "Caller:     {} recorded utterance(s)",
            options.caller_wavs.len()
        );
    } else {
        println!("Microphone: {}", device_name(&input));
    }
    println!("Speakers:   {}", device_name(&output));
    println!(
        "Session audio: {session_rate} Hz{}. Press Ctrl+C to hang up.\n",
        if options.half_duplex {
            ", half-duplex (mic muted while the AI speaks)"
        } else {
            ""
        }
    );

    let target = if options.roleplay { "9199" } else { "9198" };
    let params = CallParams {
        call_id: format!("local-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S")),
        channel_uuid: String::new(),
        target_number: target.into(),
        roleplay: options.roleplay,
        client_id: options
            .client_id
            .clone()
            .unwrap_or_else(|| settings.default_client_id.clone()),
        scenario: options.scenario.clone(),
        sample_rate: session_rate,
        can_transfer: true,
    };

    // Speaker side: play AI audio, drop it on barge-in, end the call on transfer.
    let player = {
        let (shared, event_tx) = (shared.clone(), event_tx.clone());
        tokio::spawn(async move {
            let mut resampler = Resampler::new(session_rate, speaker_rate);
            while let Some(command) = command_rx.recv().await {
                match command {
                    LegCommand::Audio(pcm) => {
                        let input: Vec<f32> =
                            bytes_to_samples(&pcm).into_iter().map(i16_to_f32).collect();
                        let mut out = Vec::with_capacity(input.len() * 2);
                        resampler.process(&input, &mut out);
                        shared.playout.lock().unwrap().extend(out);
                    }
                    LegCommand::ClearPlayout => shared.playout.lock().unwrap().clear(),
                    LegCommand::Transfer { extension } => {
                        println!("\n[transfer] The caller would now be handed to extension {extension}. Ending the local call.");
                        let _ = event_tx.send(LegEvent::Closed).await;
                    }
                }
            }
        })
    };

    let hangup = {
        let (shared, event_tx) = (shared.clone(), event_tx.clone());
        let limit = options.max_seconds;
        tokio::spawn(async move {
            let timer = async {
                match limit {
                    Some(seconds) => tokio::time::sleep(Duration::from_secs(seconds)).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = timer => {}
            }
            if !shared.stopping.swap(true, Ordering::SeqCst) {
                println!("\nHanging up...");
                let _ = event_tx.send(LegEvent::Closed).await;
            }
        })
    };

    let outcome = run_call(deps, params, event_rx, command_tx).await;
    shared.stopping.store(true, Ordering::SeqCst);
    hangup.abort();
    if let Some(caller) = caller {
        caller.abort();
    }
    let _ = player.await;
    // Let the last of the audio play out before the streams are dropped.
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(in_stream);
    drop(out_stream);
    Ok(outcome)
}
