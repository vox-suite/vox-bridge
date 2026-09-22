// this file code contains telephony provider abstractions and error types

use async_trait::async_trait;
use uuid::Uuid;

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
        action_id: Uuid,
        conversation_id: Uuid,
        opening_instruction: Option<&str>,
    ) -> Result<String, TelephonyError>;
}
