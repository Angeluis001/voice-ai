//! Mixes the caller's audio and the AI's audio into one mono WAV.
//!
//! The caller's frames drive the timeline. AI audio is queued as it is sent and is
//! mixed in, sample for sample, as caller frames arrive. Whatever AI audio is still
//! queued at the end is appended against silence.
use super::wav::{bytes_to_samples, samples_to_bytes, wrap_pcm16le};
use std::{collections::VecDeque, path::PathBuf};

const CALLER_GAIN: f32 = 0.75;
const AI_GAIN: f32 = 0.95;

pub struct MixedCallRecorder {
    path: PathBuf,
    sample_rate: u32,
    ai_queue: VecDeque<i16>,
    mixed: Vec<i16>,
    closed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub path: PathBuf,
    pub samples: usize,
    pub seconds: f64,
}

fn mix(caller: i16, ai: i16) -> i16 {
    let mixed = f32::from(caller) * CALLER_GAIN + f32::from(ai) * AI_GAIN;
    mixed.clamp(-32768.0, 32767.0) as i16
}

impl MixedCallRecorder {
    pub fn new(path: impl Into<PathBuf>, sample_rate: u32) -> Self {
        Self {
            path: path.into(),
            sample_rate,
            ai_queue: VecDeque::new(),
            mixed: Vec::new(),
            closed: false,
        }
    }

    /// Caller audio (raw PCM16LE). Advances the recording timeline.
    pub fn push_caller(&mut self, pcm: &[u8]) {
        if self.closed {
            return;
        }
        for caller in bytes_to_samples(pcm) {
            let ai = self.ai_queue.pop_front().unwrap_or(0);
            self.mixed.push(mix(caller, ai));
        }
    }

    /// AI audio (raw PCM16LE) that was sent to the caller.
    pub fn push_ai(&mut self, pcm: &[u8]) {
        if self.closed {
            return;
        }
        self.ai_queue.extend(bytes_to_samples(pcm));
    }

    /// Finish the recording and return its WAV bytes, or `None` when nothing was recorded.
    pub fn finish(&mut self) -> Option<(Recording, Vec<u8>)> {
        if self.closed {
            return None;
        }
        self.closed = true;
        while let Some(ai) = self.ai_queue.pop_front() {
            self.mixed.push(mix(0, ai));
        }
        if self.mixed.is_empty() {
            return None;
        }
        let samples = self.mixed.len();
        let wav = wrap_pcm16le(&samples_to_bytes(&self.mixed), self.sample_rate, 1);
        let recording = Recording {
            path: self.path.clone(),
            samples,
            seconds: samples as f64 / f64::from(self.sample_rate),
        };
        Some((recording, wav))
    }

    /// Finish and write the WAV file. Write errors are logged and yield `None`.
    pub async fn finalize(&mut self) -> Option<Recording> {
        let (recording, wav) = self.finish()?;
        let write = async {
            if let Some(parent) = recording.path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&recording.path, wav).await
        };
        match write.await {
            Ok(()) => Some(recording),
            Err(error) => {
                tracing::warn!(phase = "recording_write_error", path = %recording.path.display(), %error);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples_to_bytes(samples)
    }

    #[test]
    fn caller_and_ai_are_weighted_and_truncated() {
        let mut rec = MixedCallRecorder::new("x.wav", 8000);
        rec.push_ai(&pcm(&[1000, -1000]));
        rec.push_caller(&pcm(&[1000, 1000, 1001]));
        let (info, wav) = rec.finish().unwrap();
        // 1000*0.75 + 1000*0.95 = 1700; 750 - 950 = -200; 1001*0.75 = 750.75 -> 750
        assert_eq!(bytes_to_samples(&wav[44..]), vec![1700, -200, 750]);
        assert_eq!(info.samples, 3);
    }

    #[test]
    fn mixing_clamps_instead_of_wrapping() {
        let mut rec = MixedCallRecorder::new("x.wav", 8000);
        rec.push_ai(&pcm(&[32767, -32768]));
        rec.push_caller(&pcm(&[32767, -32768]));
        let (_, wav) = rec.finish().unwrap();
        assert_eq!(bytes_to_samples(&wav[44..]), vec![32767, -32768]);
    }

    #[test]
    fn leftover_ai_audio_is_flushed_against_silence() {
        let mut rec = MixedCallRecorder::new("x.wav", 8000);
        rec.push_caller(&pcm(&[100]));
        rec.push_ai(&pcm(&[1000, 2000]));
        let (info, wav) = rec.finish().unwrap();
        assert_eq!(bytes_to_samples(&wav[44..]), vec![75, 950, 1900]);
        assert_eq!(info.seconds, 3.0 / 8000.0);
    }

    #[test]
    fn empty_recording_yields_nothing_and_finish_is_once_only() {
        let mut rec = MixedCallRecorder::new("x.wav", 8000);
        assert!(rec.finish().is_none());
        let mut rec = MixedCallRecorder::new("x.wav", 8000);
        rec.push_caller(&pcm(&[1]));
        assert!(rec.finish().is_some());
        assert!(rec.finish().is_none());
    }

    #[tokio::test]
    async fn finalize_writes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("media").join("u-call.wav");
        let mut rec = MixedCallRecorder::new(&path, 8000);
        rec.push_caller(&pcm(&[1, 2, 3, 4]));
        let info = rec.finalize().await.unwrap();
        assert_eq!(info.path, path);
        assert_eq!(std::fs::read(&path).unwrap().len(), 44 + 8);
    }
}
