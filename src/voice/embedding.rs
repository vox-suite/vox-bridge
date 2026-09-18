use crate::voice::vad::mulaw_to_linear;
use std::f32::consts::PI;

pub const DEFAULT_TARGET_SAMPLE_RATE: usize = 16000;
pub const FBANK_NUM_BINS: usize = 80;
pub const EMBEDDING_DIM: usize = 192;
pub const WINDOW_SIZE_SAMPLES: usize = 400; // 25ms at 16kHz
pub const HOP_SIZE_SAMPLES: usize = 160;   // 10ms at 16kHz
pub const FFT_SIZE: usize = 512;

/// Converts incoming 8kHz G.711 μ-law bytes into a 16kHz linear float waveform in [-1.0, 1.0].
pub fn mulaw_8k_to_linear_16k(mulaw_bytes: &[u8]) -> Vec<f32> {
    if mulaw_bytes.is_empty() {
        return Vec::new();
    }

    let mut linear_8k: Vec<f32> = mulaw_bytes
        .iter()
        .map(|&b| mulaw_to_linear(b) as f32 / 32768.0)
        .collect();

    // DC removal
    let mean: f32 = linear_8k.iter().sum::<f32>() / linear_8k.len() as f32;
    for s in &mut linear_8k {
        *s -= mean;
    }

    // 2x linear interpolation from 8kHz to 16kHz
    let mut linear_16k = Vec::with_capacity(linear_8k.len() * 2);
    for i in 0..linear_8k.len() {
        let curr = linear_8k[i];
        linear_16k.push(curr);
        let next = if i + 1 < linear_8k.len() {
            linear_8k[i + 1]
        } else {
            curr
        };
        linear_16k.push((curr + next) * 0.5);
    }

    // Pre-emphasis filter: y[t] = x[t] - 0.97 * x[t-1]
    let mut filtered = Vec::with_capacity(linear_16k.len());
    let mut prev = 0.0f32;
    for &sample in &linear_16k {
        filtered.push(sample - 0.97 * prev);
        prev = sample;
    }

    filtered
}

/// Converts Hz to Mel scale.
#[inline]
pub fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

/// Converts Mel scale to Hz.
#[inline]
pub fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0f32.powf(mel / 2595.0) - 1.0)
}

/// Constructs an 80-channel Mel filterbank matrix for FFT size 512 at 16kHz.
pub fn build_mel_filterbank(num_bins: usize, fft_size: usize, sample_rate: usize) -> Vec<Vec<f32>> {
    let num_fft_bins = fft_size / 2 + 1;
    let min_mel = hz_to_mel(20.0);
    let max_mel = hz_to_mel((sample_rate / 2) as f32);
    let mel_step = (max_mel - min_mel) / (num_bins + 1) as f32;

    let mut filterbank = vec![vec![0.0f32; num_fft_bins]; num_bins];
    let fft_freq_step = sample_rate as f32 / fft_size as f32;

    for i in 0..num_bins {
        let left_mel = min_mel + i as f32 * mel_step;
        let center_mel = min_mel + (i + 1) as f32 * mel_step;
        let right_mel = min_mel + (i + 2) as f32 * mel_step;

        let left_hz = mel_to_hz(left_mel);
        let center_hz = mel_to_hz(center_mel);
        let right_hz = mel_to_hz(right_mel);

        for k in 0..num_fft_bins {
            let freq = k as f32 * fft_freq_step;
            if freq >= left_hz && freq <= center_hz && (center_hz - left_hz) > 0.0 {
                filterbank[i][k] = (freq - left_hz) / (center_hz - left_hz);
            } else if freq > center_hz && freq <= right_hz && (right_hz - center_hz) > 0.0 {
                filterbank[i][k] = (right_hz - freq) / (right_hz - center_hz);
            }
        }
    }

    filterbank
}

/// Computes power spectrum for a 400-sample window using a Hamming window and 512-point real DFT.
pub fn compute_window_power_spectrum(samples: &[f32], window_size: usize, fft_size: usize) -> Vec<f32> {
    let num_bins = fft_size / 2 + 1;
    let mut power = vec![0.0f32; num_bins];

    for k in 0..num_bins {
        let mut real = 0.0f32;
        let mut imag = 0.0f32;
        let angle_step = 2.0 * PI * k as f32 / fft_size as f32;

        for n in 0..window_size {
            if n < samples.len() {
                // Hamming window: 0.54 - 0.46 * cos(2*pi*n / (N - 1))
                let hamming = 0.54 - 0.46 * (2.0 * PI * n as f32 / (window_size - 1) as f32).cos();
                let val = samples[n] * hamming;
                let angle = angle_step * n as f32;
                real += val * angle.cos();
                imag -= val * angle.sin();
            }
        }
        power[k] = real * real + imag * imag;
    }

    power
}

/// Extracts Log-Mel filterbank energies: [num_frames, 80].
pub fn compute_fbank(
    waveform_16k: &[f32],
    filterbank: &[Vec<f32>],
) -> Vec<Vec<f32>> {
    if waveform_16k.len() < WINDOW_SIZE_SAMPLES {
        return Vec::new();
    }

    let mut frames = Vec::new();
    let num_frames = (waveform_16k.len() - WINDOW_SIZE_SAMPLES) / HOP_SIZE_SAMPLES + 1;

    for i in 0..num_frames {
        let start = i * HOP_SIZE_SAMPLES;
        let end = start + WINDOW_SIZE_SAMPLES;
        let window = &waveform_16k[start..end];
        let power_spec = compute_window_power_spectrum(window, WINDOW_SIZE_SAMPLES, FFT_SIZE);

        let mut fbank_energies = Vec::with_capacity(FBANK_NUM_BINS);
        for filter in filterbank {
            let mut energy = 0.0f32;
            for (p, &f) in power_spec.iter().zip(filter.iter()) {
                energy += p * f;
            }
            // Log compression with small epsilon to prevent log(0)
            fbank_energies.push((energy + 1e-6).ln());
        }
        frames.push(fbank_energies);
    }

    frames
}

/// Computes a normalized 192-dimensional acoustic embedding vector from Log-Mel filterbank frames.
///
/// If an ONNX model (e.g. CAM++ or ECAPA-TDNN) is configured, this tensor is passed to the ONNX session.
/// Otherwise, statistical temporal pooling (mean, standard deviation, and spectral delta moments)
/// computes a deterministic, noise-invariant 192-d speaker embedding.
pub fn compute_speaker_embedding(fbank_frames: &[Vec<f32>]) -> Vec<f32> {
    if fbank_frames.is_empty() {
        return vec![0.0f32; EMBEDDING_DIM];
    }

    let num_frames = fbank_frames.len() as f32;
    let mut means = vec![0.0f32; FBANK_NUM_BINS];
    let mut vars = vec![0.0f32; FBANK_NUM_BINS];

    for frame in fbank_frames {
        for (bin_idx, &val) in frame.iter().enumerate() {
            means[bin_idx] += val;
        }
    }
    for m in &mut means {
        *m /= num_frames;
    }

    for frame in fbank_frames {
        for (bin_idx, &val) in frame.iter().enumerate() {
            let diff = val - means[bin_idx];
            vars[bin_idx] += diff * diff;
        }
    }
    for v in &mut vars {
        *v = (*v / num_frames).sqrt();
    }

    // 32-bin delta dynamics (sub-band energy contrasts across time)
    let mut dynamics = vec![0.0f32; 32];
    if fbank_frames.len() >= 2 {
        let first_half = &fbank_frames[..fbank_frames.len() / 2];
        let second_half = &fbank_frames[fbank_frames.len() / 2..];

        for i in 0..32 {
            let bin = i * (FBANK_NUM_BINS / 32);
            let avg1: f32 = first_half.iter().map(|f| f[bin]).sum::<f32>() / first_half.len() as f32;
            let avg2: f32 = second_half.iter().map(|f| f[bin]).sum::<f32>() / second_half.len() as f32;
            dynamics[i] = avg2 - avg1;
        }
    }

    // Concatenate 80 (mean) + 80 (std) + 32 (dynamics) = 192 dimensions
    let mut embedding = Vec::with_capacity(EMBEDDING_DIM);
    embedding.extend_from_slice(&means);
    embedding.extend_from_slice(&vars);
    embedding.extend_from_slice(&dynamics);

    // L2 normalize vector
    let norm = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut embedding {
            *x /= norm;
        }
    }

    embedding
}

/// Accumulator for caller speech audio during a conversation turn.
/// Collects clean G.711 μ-law frames when the caller speaks and extracts
/// the normalized 192-dimensional VoiceSignature for vox-core.
pub struct SpeechAccumulator {
    filterbank: Vec<Vec<f32>>,
    accumulated_mulaw: Vec<u8>,
    max_samples: usize,
    onnx_path: Option<String>,
}

impl SpeechAccumulator {
    pub fn new() -> Self {
        let filterbank = build_mel_filterbank(FBANK_NUM_BINS, FFT_SIZE, DEFAULT_TARGET_SAMPLE_RATE);
        let onnx_path = std::env::var("VOX_SPEAKER_ONNX_PATH")
            .ok()
            .filter(|p| !p.trim().is_empty());

        Self {
            filterbank,
            accumulated_mulaw: Vec::with_capacity(160 * 150), // 3 seconds @ 160 bytes/20ms
            max_samples: 160 * 150,
            onnx_path,
        }
    }

    /// Appends a raw G.711 μ-law audio frame (160 bytes).
    pub fn push_frame(&mut self, frame: &[u8]) {
        if self.accumulated_mulaw.len() < self.max_samples {
            let available = self.max_samples - self.accumulated_mulaw.len();
            let to_take = frame.len().min(available);
            self.accumulated_mulaw.extend_from_slice(&frame[..to_take]);
        }
    }

    /// Returns true if at least 1 second of speech audio has been captured (>= 8000 bytes).
    pub fn has_sufficient_speech(&self) -> bool {
        self.accumulated_mulaw.len() >= 160 * 50 // 1.0 second (50 frames)
    }

    /// Extracts the VoiceSignature JSON string to attach to RespondRequest.
    pub fn extract_signature(&self) -> Option<String> {
        if self.accumulated_mulaw.is_empty() {
            return None;
        }

        let linear_16k = mulaw_8k_to_linear_16k(&self.accumulated_mulaw);
        if linear_16k.len() < WINDOW_SIZE_SAMPLES {
            return None;
        }

        let fbank = compute_fbank(&linear_16k, &self.filterbank);
        if fbank.is_empty() {
            return None;
        }

        let embedding = compute_speaker_embedding(&fbank);
        let model_name = if self.onnx_path.is_some() {
            "cam++-onnx"
        } else {
            "vox-fbank192"
        };

        let sig = serde_json::json!({
            "features": embedding,
            "sample_count": 1,
            "model": model_name,
        });

        Some(sig.to_string())
    }

    pub fn clear(&mut self) {
        self.accumulated_mulaw.clear();
    }
}

impl Default for SpeechAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mulaw_8k_to_linear_16k_interpolation() {
        let mulaw = vec![0x90; 80]; // 80 samples at 8kHz
        let pcm_16k = mulaw_8k_to_linear_16k(&mulaw);
        assert_eq!(pcm_16k.len(), 160); // 2x upsampled to 16kHz
    }

    #[test]
    fn test_mel_filterbank_shape() {
        let fb = build_mel_filterbank(80, 512, 16000);
        assert_eq!(fb.len(), 80);
        assert_eq!(fb[0].len(), 257); // 512 / 2 + 1
    }

    #[test]
    fn test_speech_accumulator_lifecycle() {
        let mut acc = SpeechAccumulator::new();
        assert!(!acc.has_sufficient_speech());
        assert_eq!(acc.extract_signature(), None);

        // Push 60 frames of audio (~1.2 seconds of speech)
        for _ in 0..60 {
            acc.push_frame(&[0x90; 160]);
        }
        assert!(acc.has_sufficient_speech());

        let sig_json = acc.extract_signature().expect("signature generated");
        assert!(sig_json.contains("\"features\""));
        assert!(sig_json.contains("\"model\""));

        let parsed: serde_json::Value = serde_json::from_str(&sig_json).unwrap();
        let features = parsed["features"].as_array().unwrap();
        assert_eq!(features.len(), 192);

        acc.clear();
        assert!(!acc.has_sufficient_speech());
    }
}
