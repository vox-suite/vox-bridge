use super::{TelephonyClient, TelephonyError};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

pub struct TwilioApiClient {
    client: Client,
    account_sid: String,
    auth_token: String,
    from_number: String,
    stream_url: String,
    status_callback_url: String,
}

#[derive(Deserialize)]
struct TwilioCallResponse {
    sid: String,
}

impl TwilioApiClient {
    pub fn new(
        account_sid: String,
        auth_token: String,
        from_number: String,
        stream_url: String,
        status_callback_url: String,
    ) -> Result<Self, TelephonyError> {
        let client = Client::builder().timeout(Duration::from_secs(15)).build()?;
        Ok(Self {
            client,
            account_sid,
            auth_token,
            from_number,
            stream_url,
            status_callback_url,
        })
    }
}

#[async_trait]
impl TelephonyClient for TwilioApiClient {
    async fn initiate_call(
        &self,
        to: &str,
        _opening_instruction: Option<&str>,
    ) -> Result<String, TelephonyError> {
        let endpoint = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls.json",
            self.account_sid
        );
        let twiml = format!(
            "<Response><Connect><Stream url=\"{}\" /></Connect></Response>",
            self.stream_url
        );
        let params = [
            ("To", to),
            ("From", &self.from_number),
            ("Twiml", &twiml),
            ("StatusCallback", &self.status_callback_url),
            ("StatusCallbackMethod", "POST"),
        ];

        let response = self
            .client
            .post(&endpoint)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&params)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(TelephonyError::Provider(format!(
                "Twilio call initiation failed: status {status}"
            )));
        }

        let body: TwilioCallResponse = response.json().await?;
        Ok(body.sid)
    }
}
