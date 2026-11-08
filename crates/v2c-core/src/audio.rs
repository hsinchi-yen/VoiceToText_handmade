//! Audio clip type shared by recorders and ASR providers.

/// All clips handed to the pipeline are 16 kHz mono.
pub const SAMPLE_RATE: u32 = 16_000;

/// A recorded utterance: 16 kHz mono f32 samples in [-1.0, 1.0].
#[derive(Clone, PartialEq)]
pub struct AudioClip {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl AudioClip {
    /// Wraps 16 kHz mono samples.
    pub fn new(samples: Vec<f32>) -> Self {
        Self {
            samples,
            sample_rate: SAMPLE_RATE,
        }
    }

    pub fn duration_ms(&self) -> u64 {
        if self.sample_rate == 0 {
            return 0;
        }
        self.samples.len() as u64 * 1000 / self.sample_rate as u64
    }

    /// Root-mean-square level, used for silence detection and the overlay level meter.
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.samples.iter().map(|s| s * s).sum();
        (sum / self.samples.len() as f32).sqrt()
    }
}

impl std::fmt::Debug for AudioClip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioClip")
            .field("sample_rate", &self.sample_rate)
            .field("duration_ms", &self.duration_ms())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_of_one_second() {
        let clip = AudioClip::new(vec![0.0; 16_000]);
        assert_eq!(clip.duration_ms(), 1000);
    }

    #[test]
    fn rms_of_silence_and_constant() {
        assert_eq!(AudioClip::new(vec![]).rms(), 0.0);
        let clip = AudioClip::new(vec![0.5; 100]);
        assert!((clip.rms() - 0.5).abs() < 1e-6);
    }
}
