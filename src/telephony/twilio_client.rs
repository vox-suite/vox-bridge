use super::{TelephonyClient, TelephonyError};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

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
        action_id: Uuid,
        conversation_id: Uuid,
        opening_instruction: Option<&str>,
    ) -> Result<String, TelephonyError> {
        let endpoint = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Calls.json",
            self.account_sid
        );
        let opening: String = opening_instruction
            .unwrap_or("Hello from Vox")
            .chars()
            .take(300)
            .collect();
        let twiml = format!(
            "<Response><Connect><Stream url=\"{}\"><Parameter name=\"action_id\" value=\"{}\"/><Parameter name=\"external_identity\" value=\"{}\"/><Parameter name=\"external_conversation_id\" value=\"{}\"/><Parameter name=\"opening_instruction\" value=\"{}\"/></Stream></Connect></Response>",
            xml_escape(&self.stream_url),
            action_id,
            xml_escape(to),
            conversation_id,
            xml_escape(&opening),
        );
        let status_callback = format!("{}?action_id={action_id}", self.status_callback_url);
        let params = [
            ("To", to),
            ("From", &self.from_number),
            ("Twiml", &twiml),
            ("StatusCallback", &status_callback),
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

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
