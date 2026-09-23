/**
* this file code contains tests for twilio protocol serialization and signature
*/
use vox_bridge::channels::twilio::protocol::{
    InboundStreamMessage, OutboundClearMessage, OutboundMark, OutboundMarkMessage,
    OutboundMediaMessage, OutboundPayload,
};
use vox_bridge::channels::twilio::signature::{
    compute_twilio_signature, validate_twilio_signature,
};

#[test]
fn serializes_outbound_media_and_mark() {
    let media = OutboundMediaMessage {
        event: "media",
        stream_sid: "stream-123",
        media: OutboundPayload {
            payload: "AQIDBA==",
        },
    };
    let json = serde_json::to_string(&media).unwrap();
    assert!(json.contains("\"event\":\"media\""));
    assert!(json.contains("\"streamSid\":\"stream-123\""));
    assert!(json.contains("\"payload\":\"AQIDBA==\""));

    let mark = OutboundMarkMessage {
        event: "mark",
        stream_sid: "stream-123",
        mark: OutboundMark { name: "mark-1" },
    };
    let json_mark = serde_json::to_string(&mark).unwrap();
    assert!(json_mark.contains("\"event\":\"mark\""));
    assert!(json_mark.contains("\"name\":\"mark-1\""));

    let clear = OutboundClearMessage {
        event: "clear",
        stream_sid: "stream-123",
    };
    let json_clear = serde_json::to_string(&clear).unwrap();
    assert!(json_clear.contains("\"event\":\"clear\""));
}

#[test]
fn deserializes_inbound_connected_and_start() {
    let connected_json = r#"{"event":"connected","protocol":"Call","version":"1.0.0"}"#;
    let msg: InboundStreamMessage = serde_json::from_str(connected_json).unwrap();
    assert!(matches!(msg, InboundStreamMessage::Connected { .. }));

    let stop_json = r#"{"event":"stop"}"#;
    let stop_msg: InboundStreamMessage = serde_json::from_str(stop_json).unwrap();
    assert!(matches!(stop_msg, InboundStreamMessage::Stop));
}

#[test]
fn validates_twilio_hmac_sha1_signature() {
    let auth_token = "secret123";
    let url = "https://example.com/bridge/twilio/voice";
    let params = vec![
        ("CallSid".to_string(), "CA12345".to_string()),
        ("From".to_string(), "+1234567890".to_string()),
    ];

    let signature = compute_twilio_signature(auth_token, url, &params);
    assert!(!signature.is_empty());

    assert!(validate_twilio_signature(
        auth_token, url, &params, &signature
    ));
    assert!(!validate_twilio_signature(
        auth_token,
        url,
        &params,
        "invalidsig"
    ));
}
