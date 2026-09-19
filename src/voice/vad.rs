//! Voice Activity Detector (VAD) for real-time telephony audio streams.
//!
//! Operates directly on G.711 μ-law (PCMU) 8kHz audio frames from Twilio,
//! detecting speech onset in <20ms to allow sub-50ms instant barge-in / interruption.

/// Converts an 8-bit G.711 μ-law sample to a 16-bit linear PCM signed integer.
#[inline]
pub fn mulaw_to_linear(u_val: u8) -> i16 {
    let u_val = !u_val;
    let sign = u_val & 0x80;
    let exponent = (u_val >> 4) & 0x07;
    let mantissa = u_val & 0x0F;
    let mut sample = ((mantissa as i16) << 3) + 0x84;
    sample <<= exponent;
    sample -= 0x84;
    if sign != 0 { -sample } else { sample }
}

/// Calculates the Root Mean Square (RMS) energy of a buffer of μ-law audio samples.
pub fn calculate_rms(mulaw_bytes: &[u8]) -> f32 {
    if mulaw_bytes.is_empty() {
        return 0.0;
    }
    let mut sum_sq: u64 = 0;
    for &b in mulaw_bytes {
        let sample = mulaw_to_linear(b) as i64;
        sum_sq += (sample * sample) as u64;
    }
    ((sum_sq as f64 / mulaw_bytes.len() as f64).sqrt()) as f32
}

/// Calculates the zero-crossing rate (ZCR) of a buffer of μ-law audio samples.
#[allow(dead_code)]
pub fn calculate_zcr(mulaw_bytes: &[u8]) -> f32 {
    if mulaw_bytes.len() < 2 {
        return 0.0;
    }
    let mut crossings = 0usize;
    let mut prev = mulaw_to_linear(mulaw_bytes[0]);
    for &b in &mulaw_bytes[1..] {
        let curr = mulaw_to_linear(b);
        if (prev >= 0 && curr < 0) || (prev < 0 && curr >= 0) {
            crossings += 1;
        }
        prev = curr;
    }
    crossings as f32 / (mulaw_bytes.len() - 1) as f32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadEvent {
    /// Background silence or ambient line noise.
    Silence,
    /// Caller began speaking (transitions from silence to active speech).
    SpeechStarted,
    /// Ongoing active speech.
    SpeechActive,
    /// Caller paused or finished speaking (hangover duration elapsed).
    SpeechEnded,
}

#[derive(Debug, Clone)]
pub struct VoiceActivityDetector {
    /// Baseline minimum RMS energy threshold for speech.
    speech_threshold: f32,
    /// Running estimation of the background noise floor.
    noise_floor: f32,
    /// Number of consecutive speech frames required to confirm speech onset (debounce).
    consecutive_onset_required: usize,
    /// Current count of consecutive speech frames observed.
    consecutive_speech_frames: usize,
    /// Number of silent frames required to transition back to silence (hangover).
    hangover_frames_required: usize,
    /// Current count of silent frames since active speech was last observed.
    silent_frame_count: usize,
    /// Whether speech is currently active.
    is_speaking: bool,
    /// Latest frame RMS energy.
    last_rms: f32,
}

impl Default for VoiceActivityDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceActivityDetector {
    pub fn new() -> Self {
        let threshold = std::env::var("VOX_VAD_THRESHOLD")
            .or_else(|_| std::env::var("VAD_THRESHOLD"))
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(650.0);

        Self {
            speech_threshold: threshold,
            noise_floor: 100.0,
            consecutive_onset_required: 2, // ~40ms at 20ms/frame
            consecutive_speech_frames: 0,
            hangover_frames_required: 12, // ~240ms hangover
            silent_frame_count: 0,
            is_speaking: false,
            last_rms: 0.0,
        }
    }

    #[allow(dead_code)]
    pub fn with_threshold(threshold: f32) -> Self {
        let mut vad = Self::new();
        vad.speech_threshold = threshold;
        vad
    }

    /// Evaluates an incoming audio frame and returns the corresponding VAD event.
    pub fn process_frame(&mut self, mulaw_audio: &[u8]) -> VadEvent {
        if mulaw_audio.is_empty() {
            return if self.is_speaking {
                VadEvent::SpeechActive
            } else {
                VadEvent::Silence
            };
        }

        let rms = calculate_rms(mulaw_audio);
        self.last_rms = rms;

        // Dynamic threshold: higher of fixed floor or adaptive noise multiplier
        let dynamic_threshold = (self.noise_floor * 2.5).max(self.speech_threshold);
        let frame_is_speech = rms >= dynamic_threshold;

        if frame_is_speech {
            self.consecutive_speech_frames += 1;
            self.silent_frame_count = 0;

            if !self.is_speaking {
                if self.consecutive_speech_frames >= self.consecutive_onset_required {
                    self.is_speaking = true;
                    tracing::info!(
                        rms = %self.last_rms,
                        threshold = %dynamic_threshold,
                        noise_floor = %self.noise_floor,
                        consecutive_frames = self.consecutive_speech_frames,
                        "VAD: Speech onset detected (SpeechStarted)"
                    );
                    VadEvent::SpeechStarted
                } else {
                    VadEvent::Silence
                }
            } else {
                VadEvent::SpeechActive
            }
        } else {
            self.consecutive_speech_frames = 0;

            if self.is_speaking {
                self.silent_frame_count += 1;
                if self.silent_frame_count >= self.hangover_frames_required {
                    self.is_speaking = false;
                    self.silent_frame_count = 0;
                    tracing::debug!(
                        noise_floor = %self.noise_floor,
                        hangover_frames = self.hangover_frames_required,
                        "VAD: Speech hangover elapsed, silence restored (SpeechEnded)"
                    );
                    VadEvent::SpeechEnded
                } else {
                    VadEvent::SpeechActive
                }
            } else {
                // Adapt noise floor during silence (exponential moving average)
                self.noise_floor = 0.95 * self.noise_floor + 0.05 * rms;
                VadEvent::Silence
            }
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub fn is_speaking(&self) -> bool {
        self.is_speaking
    }

    #[inline]
    #[allow(dead_code)]
    pub fn current_rms(&self) -> f32 {
        self.last_rms
    }

    #[inline]
    #[allow(dead_code)]
    pub fn noise_floor(&self) -> f32 {
        self.noise_floor
    }

    #[inline]
    #[allow(dead_code)]
    pub fn dynamic_threshold(&self) -> f32 {
        (self.noise_floor * 2.5).max(self.speech_threshold)
    }

    #[allow(dead_code)]
    pub fn reset(&mut self) {
        self.consecutive_speech_frames = 0;
        self.silent_frame_count = 0;
        self.is_speaking = false;
        self.last_rms = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mulaw_silence_and_extremes() {
        // 0xFF expands to 0 in standard G.711 μ-law
        assert_eq!(mulaw_to_linear(0xff), 0);
        // 0x7F is negative zero
        assert_eq!(mulaw_to_linear(0x7f), 0);
        // 0x00 is max negative
        assert_eq!(mulaw_to_linear(0x00), -32124);
        // 0x80 is max positive
        assert_eq!(mulaw_to_linear(0x80), 32124);
    }

    #[test]
    fn test_rms_calculation() {
        let silence = [0xff; 160];
        assert_eq!(calculate_rms(&silence), 0.0);

        let max_signal = [0x80; 160];
        assert_eq!(calculate_rms(&max_signal), 32124.0);
    }

    #[test]
    fn test_vad_speech_onset_and_hangover() {
        let mut vad = VoiceActivityDetector::with_threshold(400.0);

        let silence_frame = [0xff; 160];
        // Low speech frame: ~800 RMS (0x90 expands to ~2000 linear)
        let speech_frame = [0x90; 160];

        // 1. Initial silence
        assert_eq!(vad.process_frame(&silence_frame), VadEvent::Silence);
        assert!(!vad.is_speaking());

        // 2. Single frame of speech: not yet started due to onset debounce (needs 2 frames)
        assert_eq!(vad.process_frame(&speech_frame), VadEvent::Silence);
        assert!(!vad.is_speaking());

        // 3. Second consecutive frame: speech started!
        assert_eq!(vad.process_frame(&speech_frame), VadEvent::SpeechStarted);
        assert!(vad.is_speaking());

        // 4. Continued speech
        assert_eq!(vad.process_frame(&speech_frame), VadEvent::SpeechActive);
        assert!(vad.is_speaking());

        // 5. Silence frames within hangover
        for _ in 0..11 {
            assert_eq!(vad.process_frame(&silence_frame), VadEvent::SpeechActive);
            assert!(vad.is_speaking());
        }

        // 6. 12th silent frame: hangover expires, speech ended
        assert_eq!(vad.process_frame(&silence_frame), VadEvent::SpeechEnded);
        assert!(!vad.is_speaking());

        // 7. Subsequent frames are silence
        assert_eq!(vad.process_frame(&silence_frame), VadEvent::Silence);
    }
}
