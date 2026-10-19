use vox_bridge::channels::context::CallContext;

#[test]
fn builds_call_context_identity() {
    let ctx = CallContext {
        channel: "voice".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "twilio:CA12345".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: Some("elevenlabs".into()),
        outbound: false,
        usage: Default::default(),
    };
    assert_eq!(ctx.channel, "voice");
    assert_eq!(ctx.external_identity, "+15551234567");
}
