/**
* this file code contains twilio telephony client implementation
*/
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

use crate::providers::telephony::{TelephonyClient, TelephonyError};

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
            r#"<Response><Connect><Stream url="{}"><Parameter name="action_id" value="{}" /><Parameter name="conversation_id" value="{}" /><Parameter name="opening_instruction" value="{}" /></Stream></Connect></Response>"#,
            self.stream_url,
            action_id,
            conversation_id,
            xml_escape(&opening),
        );

        let params = [
            ("To", to),
            ("From", &self.from_number),
            ("Twiml", &twiml),
            ("StatusCallback", &self.status_callback_url),
            ("StatusCallbackEvent", "initiated"),
            ("StatusCallbackEvent", "ringing"),
            ("StatusCallbackEvent", "answered"),
            ("StatusCallbackEvent", "completed"),
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
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "no body".to_string());
            return Err(TelephonyError::Provider(format!(
                "Twilio API error {status}: {body}"
            )));
        }

        let parsed: TwilioCallResponse = response.json().await?;
        Ok(parsed.sid)
    }
}

pub fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
