//! Minimal in-memory WAV (RIFF, PCM16 mono) encoder for cloud ASR uploads.
//!
//! TODO(audio.rs owner): move next to `AudioClip` once `audio.rs` is free to edit.

use crate::audio::AudioClip;

/// Encodes the clip as a 16-bit PCM mono WAV file. Samples are clamped to [-1.0, 1.0].
pub fn encode_wav_pcm16(clip: &AudioClip) -> Vec<u8> {
    let data_len = (clip.samples.len() * 2) as u32;
    let byte_rate = clip.sample_rate * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&clip.sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in &clip.samples {
        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// A generated test clip (440 Hz tone at low volume) for connection probes.
pub fn probe_clip(duration_ms: u64) -> AudioClip {
    let n = (crate::audio::SAMPLE_RATE as u64 * duration_ms / 1000) as usize;
    let rate = crate::audio::SAMPLE_RATE as f32;
    let samples = (0..n)
        .map(|i| 0.1 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate).sin())
        .collect();
    AudioClip::new(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }

    #[test]
    fn header_and_length() {
        let clip = AudioClip::new(vec![0.0, 1.0, -1.0, 2.0]);
        let w = encode_wav_pcm16(&clip);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(u32_at(&w, 4), 36 + 8);
        assert_eq!(u32_at(&w, 24), 16_000);
        assert_eq!(u32_at(&w, 28), 32_000);
        assert_eq!(&w[36..40], b"data");
        assert_eq!(u32_at(&w, 40), 8);
        assert_eq!(w.len(), 44 + 8);
        let s = |i: usize| i16::from_le_bytes([w[44 + 2 * i], w[45 + 2 * i]]);
        assert_eq!(s(0), 0);
        assert_eq!(s(1), i16::MAX);
        assert_eq!(s(2), -i16::MAX);
        assert_eq!(s(3), i16::MAX, "clamped");
    }

    #[test]
    fn probe_clip_duration() {
        let c = probe_clip(3000);
        assert_eq!(c.duration_ms(), 3000);
        assert!(c.rms() > 0.01);
    }
}
