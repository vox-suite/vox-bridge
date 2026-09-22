// this file code contains core authentication and host assertion signing

use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::voice::provider::VoiceError;

type HmacSha256 = Hmac<Sha256>;

pub fn canonical_assertion(
    credential_id: Uuid,
    audience: &str,
    issued_at: i64,
    nonce: Uuid,
    host_user_id: &str,
) -> String {
    let mut canonical = String::from("vox-host-assertion-v1");
    for field in [
        credential_id.to_string(),
        audience.to_owned(),
        issued_at.to_string(),
        nonce.to_string(),
        host_user_id.to_owned(),
        String::new(),
    ] {
        canonical.push('|');
        canonical.push_str(&field.len().to_string());
        canonical.push(':');
        canonical.push_str(&field);
    }
    canonical
}

pub fn sign_assertion(
    secret: &str,
    credential_id: Uuid,
    audience: &str,
    issued_at: i64,
    nonce: Uuid,
    host_user_id: &str,
) -> Result<String, VoiceError> {
    let canonical = canonical_assertion(credential_id, audience, issued_at, nonce, host_user_id);
    let mut signer = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| VoiceError::Configuration("Core host credential is invalid".into()))?;
    signer.update(canonical.as_bytes());
    Ok(hex::encode(signer.finalize().into_bytes()))
}
