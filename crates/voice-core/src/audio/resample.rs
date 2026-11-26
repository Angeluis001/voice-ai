//! Streaming mono resampler for speech (linear interpolation, with a moving-average
//! pre-filter when downsampling to limit aliasing).
use std::collections::VecDeque;

pub struct Resampler {
    /// Input samples consumed per output sample.
    step: f64,
    /// Position of the next output sample, relative to `prev` (0.0) and the next input (1.0).
    pos: f64,
    prev: f32,
    window: VecDeque<f32>,
    window_len: usize,
    window_sum: f32,
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let step = f64::from(in_rate) / f64::from(out_rate);
        let window_len = if step > 1.0 { step.round() as usize } else { 1 };
        Self {
            step,
            pos: 0.0,
            prev: 0.0,
            window: VecDeque::new(),
            window_len,
            window_sum: 0.0,
        }
    }

    fn smooth(&mut self, sample: f32) -> f32 {
        if self.window_len <= 1 {
            return sample;
        }
        self.window.push_back(sample);
        self.window_sum += sample;
        if self.window.len() > self.window_len {
            self.window_sum -= self.window.pop_front().unwrap_or(0.0);
        }
        self.window_sum / self.window.len() as f32
    }

    /// Feed input samples; resampled output is appended to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &raw in input {
            let sample = self.smooth(raw);
            while self.pos < 1.0 {
                out.push(self.prev + (sample - self.prev) * self.pos as f32);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = sample;
        }
    }
}

pub fn f32_to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0) as i16
}

pub fn i16_to_f32(sample: i16) -> f32 {
    f32::from(sample) / 32768.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(in_rate: u32, out_rate: u32, input: &[f32], chunk: usize) -> Vec<f32> {
        let mut r = Resampler::new(in_rate, out_rate);
        let mut out = vec![];
        for part in input.chunks(chunk) {
            r.process(part, &mut out);
        }
        out
    }

    #[test]
    fn same_rate_is_a_one_sample_delay() {
        let out = run(8000, 8000, &[0.1, 0.2, 0.3], 3);
        assert_eq!(out, vec![0.0, 0.1, 0.2]);
    }

    #[test]
    fn output_length_follows_the_rate_ratio() {
        let input = vec![0.5f32; 48000];
        assert!((run(48000, 24000, &input, 480).len() as i64 - 24000).abs() <= 1);
        assert!((run(48000, 8000, &input, 441).len() as i64 - 8000).abs() <= 1);
        assert!((run(24000, 48000, &input[..24000], 240).len() as i64 - 48000).abs() <= 2);
        assert!((run(44100, 24000, &input[..44100], 512).len() as i64 - 24000).abs() <= 1);
    }

    #[test]
    fn chunking_does_not_change_the_result() {
        let input: Vec<f32> = (0..2000).map(|i| ((i as f32) * 0.05).sin()).collect();
        assert_eq!(
            run(48000, 24000, &input, 7),
            run(48000, 24000, &input, 2000)
        );
        assert_eq!(
            run(24000, 48000, &input, 13),
            run(24000, 48000, &input, 2000)
        );
    }

    #[test]
    fn steady_level_is_preserved() {
        let out = run(48000, 24000, &vec![0.25f32; 4800], 480);
        assert!(out[100..].iter().all(|s| (s - 0.25).abs() < 1e-4));
        let up = run(24000, 48000, &vec![0.25f32; 2400], 240);
        assert!(up[100..].iter().all(|s| (s - 0.25).abs() < 1e-4));
    }

    #[test]
    fn sample_conversion_clamps() {
        assert_eq!(f32_to_i16(2.0), 32767);
        assert_eq!(f32_to_i16(-2.0), -32767);
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(i16_to_f32(-32768), -1.0);
    }
}
