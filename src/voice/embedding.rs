use crate::voice::vad::{calculate_rms, mulaw_to_linear};
use ort::{session::Session, value::Tensor};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use sha2::{Digest, Sha256};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

const MIN_SAMPLES: usize = 8000;
const MAX_SAMPLES: usize = 8000 * 5;
const FFT_SIZE: usize = 512;
const BINS: usize = 80;

#[derive(Default)]
pub struct SpeechAccumulator {
    accumulated_mulaw: Vec<u8>,
}

impl SpeechAccumulator {
    pub fn new() -> Self { Self::default() }

    pub fn push_frame(&mut self, frame: &[u8]) {
        if calculate_rms(frame) < 250.0 { return; }
        let frame = &frame[frame.len().saturating_sub(MAX_SAMPLES)..];
        let excess = (self.accumulated_mulaw.len() + frame.len()).saturating_sub(MAX_SAMPLES);
        self.accumulated_mulaw.drain(..excess);
        self.accumulated_mulaw.extend_from_slice(frame);
    }

    pub fn take_audio(&mut self) -> Vec<u8> { std::mem::take(&mut self.accumulated_mulaw) }
}

struct SpeakerModel {
    session: Mutex<Session>,
    identity: String,
    fbank: bool,
}

static MODEL: OnceLock<Option<SpeakerModel>> = OnceLock::new();

pub fn initialize_speaker_model() -> Result<(), String> {
    if MODEL.get().is_some() { return Ok(()); }
    let path = std::env::var("VOX_SPEAKER_ONNX_PATH").ok().filter(|p| !p.trim().is_empty());
    let model = match path {
        Some(path) => Some(SpeakerModel::load(&path, &std::env::var("VOX_SPEAKER_INPUT").unwrap_or_else(|_| "waveform".into()))?),
        None => None,
    };
    tracing::info!(enabled = model.is_some(), "VOICE_SPEAKER_MODEL_INITIALIZED");
    let _ = MODEL.set(model);
    Ok(())
}

impl SpeakerModel {
    fn load(path: &str, input: &str) -> Result<Self, String> {
        if !matches!(input, "waveform" | "fbank") { return Err("VOX_SPEAKER_INPUT must be waveform or fbank".into()); }
        let bytes = std::fs::read(path).map_err(|e| format!("speaker model file: {e}"))?;
        let identity = format!("onnx-sha256:{:x}", Sha256::digest([bytes.as_slice(), input.as_bytes(), b"vox-audio-v2"].concat()));
        let session = Session::builder().and_then(|builder| builder.with_intra_threads(1))
            .and_then(|builder| builder.commit_from_memory(&bytes)).map_err(|e| format!("speaker model load: {e}"))?;
        if session.inputs.len() != 1 || session.outputs.len() != 1 { return Err("speaker model must have one float input and one embedding output".into()); }
        Ok(Self { session: Mutex::new(session), identity, fbank: input == "fbank" })
    }

    fn extract(&self, audio: &[u8]) -> Result<Option<String>, String> {
        if audio.len() < MIN_SAMPLES { return Ok(None); }
        let waveform = mulaw_8k_to_linear_16k(audio);
        let (shape, values) = if self.fbank {
            let values = compute_fbank(&waveform);
            (vec![1, values.len() / BINS, BINS], values)
        } else { (vec![1, waveform.len()], waveform) };
        let input = Tensor::from_array((shape, values)).map_err(|e| e.to_string())?;
        let mut session = self.session.lock().map_err(|_| "speaker session poisoned")?;
        let outputs = session.run(ort::inputs![input]).map_err(|e| e.to_string())?;
        let (_, features) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
        let Some(features) = normalize_embedding(features) else { return Ok(None); };
        Ok(Some(serde_json::json!({
            "features": features, "sample_count": 1,
            "sample_duration_ms": audio.len() / 8, "model": self.identity,
        }).to_string()))
    }
}

pub fn extract_audio_signature(audio: Vec<u8>) -> Option<String> {
    if audio.len() < MIN_SAMPLES { return None; }
    let model = MODEL.get()?.as_ref()?;
    match model.extract(&audio) {
        Ok(signature) => signature,
        Err(error) => { tracing::warn!(%error, "VOICE_EMBEDDING_INCONCLUSIVE"); None }
    }
}

fn normalize_embedding(features: &[f32]) -> Option<Vec<f32>> {
    if features.len() < 16 || features.len() > 4096 || features.iter().any(|v| !v.is_finite()) { return None; }
    let norm = features.iter().map(|v| (*v as f64).powi(2)).sum::<f64>().sqrt();
    if norm < 1e-12 || !norm.is_finite() { return None; }
    Some(features.iter().map(|v| (*v as f64 / norm) as f32).collect())
}

pub fn mulaw_8k_to_linear_16k(audio: &[u8]) -> Vec<f32> {
    let mut result = Vec::with_capacity(audio.len() * 2);
    for (i, &byte) in audio.iter().enumerate() {
        let current = mulaw_to_linear(byte) as f32 / 32768.0;
        let next = mulaw_to_linear(*audio.get(i + 1).unwrap_or(&byte)) as f32 / 32768.0;
        result.push(current);
        result.push((current + next) * 0.5);
    }
    result
}

struct Fbank {
    fft: Arc<dyn Fft<f32>>,
    filters: Vec<Vec<f32>>,
    window: Vec<f32>,
}

static FBANK: LazyLock<Fbank> = LazyLock::new(|| {
    let mel = |hz: f32| 1127.0 * (1.0 + hz / 700.0).ln();
    let low = mel(20.0);
    let step = (mel(8000.0) - low) / (BINS + 1) as f32;
    let filters = (0..BINS).map(|bin| {
        let left = low + bin as f32 * step;
        let center = left + step;
        let right = center + step;
        (0..=FFT_SIZE / 2).map(|k| {
            let frequency = mel(k as f32 * 16000.0 / FFT_SIZE as f32);
            ((frequency - left) / (center - left)).min((right - frequency) / (right - center)).max(0.0)
        }).collect()
    }).collect();
    let window = (0..400).map(|i| (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / 399.0).cos()).powf(0.85)).collect();
    Fbank { fft: FftPlanner::new().plan_fft_forward(FFT_SIZE), filters, window }
});

fn compute_fbank(waveform: &[f32]) -> Vec<f32> {
    let mut output = Vec::new();
    let mut spectrum = vec![Complex::new(0.0, 0.0); FFT_SIZE];
    let mut scratch = vec![Complex::new(0.0, 0.0); FBANK.fft.get_inplace_scratch_len()];
    for start in (0..waveform.len().saturating_sub(399)).step_by(160) {
        let frame = &waveform[start..start + 400];
        let mean = frame.iter().sum::<f32>() / 400.0;
        spectrum.fill(Complex::new(0.0, 0.0));
        for i in 0..400 {
            let current = (frame[i] - mean) * 32768.0;
            let previous = (frame[i.saturating_sub(1)] - mean) * 32768.0;
            spectrum[i].re = (current - 0.97 * previous) * FBANK.window[i];
        }
        FBANK.fft.process_with_scratch(&mut spectrum, &mut scratch);
        for filter in &FBANK.filters {
            let power = filter.iter().zip(&spectrum).map(|(weight, value)| weight * value.norm_sqr()).sum::<f32>();
            output.push(power.max(f32::EPSILON).ln());
        }
    }
    let frames = output.len() / BINS;
    if frames > 0 {
        for bin in 0..BINS {
            let mean = (0..frames).map(|frame| output[frame * BINS + bin]).sum::<f32>() / frames as f32;
            for frame in 0..frames { output[frame * BINS + bin] -= mean; }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_samples_and_unconfigured_model_never_produce_synthetic_identity() {
        assert!(extract_audio_signature(vec![0x90; 7999]).is_none());
    }

    #[test]
    fn rejects_invalid_embeddings() {
        assert!(normalize_embedding(&[0.0; 192]).is_none());
        assert!(normalize_embedding(&[f32::NAN; 192]).is_none());
        assert!(normalize_embedding(&[f32::INFINITY; 192]).is_none());
        let normalized = normalize_embedding(&[2.0; 192]).unwrap();
        assert!((normalized.iter().map(|v| v * v).sum::<f32>() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn snapshot_releases_audio_and_bounds_large_frames() {
        let mut accumulator = SpeechAccumulator::new();
        accumulator.push_frame(&vec![0x90; MAX_SAMPLES + 160]);
        assert_eq!(accumulator.take_audio().len(), MAX_SAMPLES);
        assert!(accumulator.take_audio().is_empty());
        accumulator.push_frame(&[0xff; 160]);
        assert!(accumulator.take_audio().is_empty());
    }

    #[test]
    fn fbank_has_expected_shape_and_temporal_mean_normalization() {
        let waveform: Vec<f32> = (0..16000).map(|i| (i as f32 * 0.07).sin()).collect();
        let features = compute_fbank(&waveform);
        assert_eq!(features.len(), 98 * BINS);
        assert!(features.iter().all(|v| v.is_finite()));
        for bin in 0..BINS {
            let mean = (0..98).map(|frame| features[frame * BINS + bin]).sum::<f32>() / 98.0;
            assert!(mean.abs() < 0.0001);
        }
    }
}
