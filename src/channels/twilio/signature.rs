use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

pub(super) fn validate_twilio_signature(
    auth_token: &str,
    public_url: &str,
    form_parameters: &[(String, String)],
    provided_signature: &str,
) -> bool {
    let Ok(decoded_signature) = STANDARD.decode(provided_signature) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(auth_token.as_bytes()) else {
        return false;
    };

    let mut parameters: Vec<_> = form_parameters.iter().collect();
    parameters.sort_unstable();
    parameters.dedup();

    mac.update(public_url.as_bytes());
    for (name, value) in parameters {
        mac.update(name.as_bytes());
        mac.update(value.as_bytes());
    }

    mac.verify_slice(&decoded_signature).is_ok()
}
