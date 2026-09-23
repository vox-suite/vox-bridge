/**
* this file code contains tests for audio transcoding and mulaw encoding
*/
use vox_bridge::voice::audio::mp3::{linear_to_mulaw, mulaw_to_linear};

#[test]
fn mulaw_and_linear_roundtrip() {
    for val in [-30000, -15000, -1000, -50, 0, 50, 1000, 15000, 30000] {
        let encoded = linear_to_mulaw(val);
        let decoded = mulaw_to_linear(encoded);
        let diff = (val - decoded).abs();
        assert!(diff < 2000, "original: {val}, decoded: {decoded}");
    }
}
