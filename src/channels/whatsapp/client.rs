/**
* this file code contains whatsapp message delivery client
*/
pub async fn send_whatsapp_message(
    token: &str,
    phone_id: &str,
    receiver_number: &str,
    msg: &str,
) -> Result<(), reqwest::Error> {
    let url = format!("https://graph.facebook.com/v25.0/{phone_id}/messages");
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

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(%status, %body, "WhatsApp send failed");
    }

    Ok(())
}
