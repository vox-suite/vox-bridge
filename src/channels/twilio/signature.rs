/**
* this file code contains twilio webhook signature validation
*/
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

pub fn validate_twilio_signature(
    auth_token: &str,
    public_url: &str,
    form_parameters: &[(String, String)],
    provided_signature: &str,
) -> bool {
    let mut parameters = form_parameters.to_vec();
    parameters.sort_by(|left, right| left.0.cmp(&right.0));

    let mut signed_data = public_url.to_owned();
    for (name, value) in parameters {
        signed_data.push_str(&name);
        signed_data.push_str(&value);
    }

    let Ok(mut mac) = HmacSha1::new_from_slice(auth_token.as_bytes()) else {
        return false;
    };
    mac.update(signed_data.as_bytes());

    let Ok(decoded_provided_signature) = STANDARD.decode(provided_signature.as_bytes()) else {
        return false;
    };

    mac.verify_slice(&decoded_provided_signature).is_ok()
}

pub fn compute_twilio_signature(
    auth_token: &str,
    public_url: &str,
    form_parameters: &[(String, String)],
) -> String {
    let mut parameters = form_parameters.to_vec();
    parameters.sort_by(|left, right| left.0.cmp(&right.0));

    let mut signed_data = public_url.to_owned();
    for (name, value) in parameters {
        signed_data.push_str(&name);
        signed_data.push_str(&value);
    }

    let mut mac =
        HmacSha1::new_from_slice(auth_token.as_bytes()).expect("HMAC accepts keys of any length");
    mac.update(signed_data.as_bytes());
    STANDARD.encode(mac.finalize().into_bytes())
}
