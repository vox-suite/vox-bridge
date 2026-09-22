// this file code contains tests for vox core protocol and authentication

use uuid::Uuid;
use vox_bridge::channels::context::CallContext;
use vox_bridge::core::auth::{canonical_assertion, sign_assertion};

#[test]
fn signs_canonical_assertion_with_hmac_sha256() {
    let credential_id = Uuid::new_v4();
    let audience = "vox-host:development:bridge";
    let issued_at = 1700000000;
    let nonce = Uuid::new_v4();
    let host_user_id = "+15551234567";
    let host_secret = "secret-key";

    let assertion = canonical_assertion(
        credential_id,
        audience,
        issued_at,
        nonce,
        host_user_id,
    );
    assert!(assertion.starts_with("vox-host-assertion-v1|"));
    assert!(assertion.contains(audience));
    assert!(assertion.contains(host_user_id));

    let signature = sign_assertion(
        host_secret,
        credential_id,
        audience,
        issued_at,
        nonce,
        host_user_id,
    )
    .unwrap();
    assert!(!signature.is_empty());
}

#[test]
fn builds_call_context_identity() {
    let ctx = CallContext {
        channel: "voice".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "twilio:CA12345".into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: None,
        revision: None,
        tts_provider: Some("elevenlabs".into()),
    };
    assert_eq!(ctx.channel, "voice");
    assert_eq!(ctx.external_identity, "+15551234567");
}
