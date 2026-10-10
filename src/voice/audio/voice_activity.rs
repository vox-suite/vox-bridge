pub struct VoiceActivity {
    detector: earshot::Detector,
    frame: Vec<i16>,
    onset: usize,
    active: bool,
    voiced: usize,
    silence: usize,
    previous: i16,
}

#[derive(Default)]
pub struct Activity {
    pub started: bool,
    pub voiced: bool,
    pub ended: bool,
}

impl Default for VoiceActivity {
    fn default() -> Self {
        Self {
            detector: earshot::Detector::default(),
            frame: Vec::with_capacity(256),
            onset: 0,
            active: false,
            voiced: 0,
            silence: 0,
            previous: 0,
        }
    }
}

impl VoiceActivity {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn push_mulaw(&mut self, bytes: &[u8]) -> Activity {
        let mut event = Activity::default();
        for &byte in bytes {
            let value = !byte;
            let magnitude = (((value & 15) as i32 * 8 + 132) << ((value >> 4) & 7)) - 132;
            let sample = if value & 128 != 0 {
                -magnitude
            } else {
                magnitude
            } as i16;
            for sample in [((self.previous as i32 + sample as i32) / 2) as i16, sample] {
                self.frame.push(sample);
                if self.frame.len() != 256 {
                    continue;
                }
                let speech = self.detector.predict_i16(&self.frame) >= 0.5;
                event.voiced |= speech;
                if !self.active {
                    self.onset = if speech { self.onset + 1 } else { 0 };
                    if self.onset >= 3 {
                        self.active = true;
                        self.voiced = self.onset;
                        self.silence = 0;
                        event.started = true;
                    }
                } else if speech {
                    self.voiced += 1;
                    self.silence = 0;
                } else {
                    self.silence += 1;
                    if self.silence >= 40 {
                        event.ended |= self.voiced >= 10;
                        self.active = false;
                        self.onset = 0;
                    }
                }
                self.frame.clear();
            }
            self.previous = sample;
        }
        event
    }
}
