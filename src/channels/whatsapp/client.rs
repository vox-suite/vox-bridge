/**
* this file code contains whatsapp message delivery client
*/
use std::time::Duration;

use crate::retry::retry_with_backoff;

pub async fn send_whatsapp_message(
    token: &str,
    phone_id: &str,
    receiver_number: &str,
    msg: &str,
) -> Result<(), reqwest::Error> {
    let url = format!("https://graph.facebook.com/v25.0/{phone_id}/messages");

    retry_with_backoff(3, Duration::from_millis(300), || async {
        let response = reqwest::Client::new()
            .post(&url)
            .bearer_auth(token)
            .json(&serde_json::json!({
                "messaging_product": "whatsapp",
                "to": receiver_number,
                "type": "text",
                "text": {
                    "body": msg
                }
            }))
            .send()
            .await?;

        if let Err(err) = response.error_for_status_ref() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(%status, %body, "WhatsApp send failed");
            return Err(err);
        }

        Ok(())
    })
    .await
}
