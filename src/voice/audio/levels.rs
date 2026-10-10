/// Passive mu-law energy estimate. This never changes turn detection or playback.
pub fn has_voice_energy(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    let energy: u64 = bytes
        .iter()
        .map(|&byte| {
            let value = !byte;
            let magnitude = (((value & 0x0f) as i32) * 8 + 132) << ((value & 0x70) >> 4);
            let sample = magnitude - 132;
            (sample as i64 * sample as i64) as u64
        })
        .sum();
    energy / bytes.len() as u64 > 200 * 200
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_does_not_extend_speech_end() {
        assert!(!has_voice_energy(&[0xff; 160]));
        assert!(!has_voice_energy(&[0x7f; 160]));
        assert!(!has_voice_energy(&[]));
        assert!(has_voice_energy(&[0x80; 160]));
    }
}
