pub mod twilio_client;

use async_trait::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum TelephonyError {
    #[error("telephony provider request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("telephony provider returned error: {0}")]
    Provider(String),
}

#[async_trait]
pub trait TelephonyClient: Send + Sync {
    async fn initiate_call(
        &self,
        to: &str,
        opening_instruction: Option<&str>,
    ) -> Result<String, TelephonyError>;
}
