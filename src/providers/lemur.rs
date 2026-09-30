/**
* this file code contains AssemblyAI LeMUR (Large Language Models for Audio) client.
* LeMUR distills unstructured speech transcripts into structured Vox Action Proposals,
* Spans calendar entries, and verified conversational summaries.
*/
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::voice::provider::VoiceError;

#[derive(Clone)]
pub struct AssemblyAiLemur {
    api_key: String,
    client: reqwest::Client,
    base_url: String,
}

#[derive(Serialize)]
struct LemurTaskPayload<'a> {
    input_text: &'a str,
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_model: Option<&'a str>,
}

#[derive(Deserialize)]
struct LemurTaskApiResponse {
    response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedActionProposal {
    pub title: String,
    pub provider: String,
    pub capability: String,
    pub material_details: serde_json::Value,
    pub currency: Option<String>,
    pub amount: Option<f64>,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExtractedSpan {
    pub title: String,
    pub summary: String,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub category: String,
}

impl AssemblyAiLemur {
    pub fn new(api_key: String, client: reqwest::Client) -> Self {
        Self {
            api_key,
            client,
            base_url: "https://api.assemblyai.com/lemur/v3".into(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    fn headers(&self) -> Result<HeaderMap, VoiceError> {
        let mut headers = HeaderMap::new();
        let auth_val = HeaderValue::from_str(&self.api_key)
            .map_err(|_| VoiceError::Configuration("ASSEMBLYAI_API_KEY is invalid".into()))?;
        headers.insert(AUTHORIZATION, auth_val);
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    /// Execute a generic LeMUR task prompt against a speech transcript.
    pub async fn query_lemur(
        &self,
        transcript: &str,
        prompt: &str,
        context: Option<&str>,
    ) -> Result<String, VoiceError> {
        if transcript.trim().is_empty() {
            return Ok(String::new());
        }

        let endpoint = format!("{}/generate/task", self.base_url);
        let payload = LemurTaskPayload {
            input_text: transcript,
            prompt,
            context,
            final_model: Some("default"),
        };

        let response = self
            .client
            .post(&endpoint)
            .headers(self.headers()?)
            .timeout(Duration::from_secs(30))
            .json(&payload)
            .send()
            .await
            .map_err(|e| VoiceError::Provider {
                provider: "assemblyai_lemur",
                message: format!("LeMUR HTTP request failed: {e}"),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(VoiceError::Provider {
                provider: "assemblyai_lemur",
                message: format!("LeMUR API returned HTTP {status}: {body}"),
            });
        }

        let result: LemurTaskApiResponse =
            response.json().await.map_err(|e| VoiceError::Provider {
                provider: "assemblyai_lemur",
                message: format!("Failed to parse LeMUR response: {e}"),
            })?;

        Ok(result.response)
    }

    /// Uses LeMUR to convert spoken dialogue into structured Vox Action Proposals.
    /// These proposals adhere to Vox's exact single-use approval invariants.
    pub async fn extract_action_proposals(
        &self,
        transcript: &str,
    ) -> Result<Vec<ExtractedActionProposal>, VoiceError> {
        const PROMPT: &str = r#"
You are the cognitive action extraction engine for the Vox AI Agent platform.
Analyze this voice conversation between a user and the Vox agent.
Identify any explicit user requests or commitments that require consequential external execution
(e.g., booking travel/hotels, ordering food, ordering a ride, making an expense, or scheduling a calendar event).

For each requested action, output a JSON array of objects with the following schema:
[
  {
    "title": "Short action summary (e.g., Book Hyatt Seattle)",
    "provider": "Service provider if mentioned (e.g., expedia, uber, zomato, google_calendar)",
    "capability": "Action capability (e.g., book_lodging, ride_request, food_order, create_event)",
    "material_details": { "exact_parameters": "key details extracted from speech" },
    "currency": "USD or INR if mentioned",
    "amount": numeric price if mentioned,
    "requires_approval": true
  }
]
Return ONLY a valid JSON array. If no consequential actions are requested, return [].
"#;

        let raw = self
            .query_lemur(transcript, PROMPT, Some("Vox Core Action Extraction"))
            .await?;
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }

        // Clean any markdown code blocks returned by LLM
        let cleaned = clean_json_fences(&raw);
        serde_json::from_str::<Vec<ExtractedActionProposal>>(cleaned).map_err(|e| {
            VoiceError::Provider {
                provider: "assemblyai_lemur",
                message: format!("Failed to parse LeMUR action proposals JSON: {e} (raw: {raw})"),
            }
        })
    }

    /// Uses LeMUR to extract Spans (calendar timeblocks and structured agendas) from conversation.
    pub async fn extract_spans(&self, transcript: &str) -> Result<Vec<ExtractedSpan>, VoiceError> {
        const PROMPT: &str = r#"
Analyze the following conversation and extract all scheduled events, commitments, or focus blocks mentioned.
Output a JSON array of objects with this schema:
[
  {
    "title": "Event or span title",
    "summary": "1-2 sentence description",
    "start_time": "ISO 8601 or natural language time if mentioned",
    "end_time": "ISO 8601 or natural language time if mentioned",
    "category": "work | personal | travel | focus"
  }
]
Return ONLY a valid JSON array. If none, return [].
"#;

        let raw = self
            .query_lemur(transcript, PROMPT, Some("Vox Desktop Spans Extraction"))
            .await?;
        if raw.trim().is_empty() {
            return Ok(Vec::new());
        }

        let cleaned = clean_json_fences(&raw);
        serde_json::from_str::<Vec<ExtractedSpan>>(cleaned).map_err(|e| VoiceError::Provider {
            provider: "assemblyai_lemur",
            message: format!("Failed to parse LeMUR spans JSON: {e} (raw: {raw})"),
        })
    }

    /// Uses LeMUR to generate an authoritative conversation summary for audit logs.
    pub async fn summarize_call(&self, transcript: &str) -> Result<String, VoiceError> {
        const PROMPT: &str = "Provide a concise, factual 2-sentence summary of what the user and assistant discussed, agreed upon, or executed.";
        self.query_lemur(transcript, PROMPT, Some("Vox Conversation Summary"))
            .await
    }
}

fn clean_json_fences(raw: &str) -> &str {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed.strip_prefix("```json") {
        if let Some(inner) = stripped.strip_suffix("```") {
            return inner.trim();
        }
    } else if let Some(stripped) = trimmed.strip_prefix("```")
        && let Some(inner) = stripped.strip_suffix("```")
    {
        return inner.trim();
    }
    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_json_fences() {
        let wrapped = "```json\n[{\"title\":\"Test\"}]\n```";
        assert_eq!(clean_json_fences(wrapped), "[{\"title\":\"Test\"}]");

        let plain = "[{\"title\":\"Test\"}]";
        assert_eq!(clean_json_fences(plain), "[{\"title\":\"Test\"}]");
    }

    #[test]
    fn test_deserialize_extracted_proposal() {
        let json_data = r#"[
            {
                "title": "Book Hyatt Regency Seattle",
                "provider": "expedia",
                "capability": "book_lodging",
                "material_details": {
                    "city": "Seattle",
                    "hotel": "Hyatt Regency",
                    "check_in": "2026-09-29"
                },
                "currency": "USD",
                "amount": 219.0,
                "requires_approval": true
            }
        ]"#;

        let proposals: Vec<ExtractedActionProposal> = serde_json::from_str(json_data).unwrap();
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "Book Hyatt Regency Seattle");
        assert_eq!(proposals[0].amount, Some(219.0));
        assert!(proposals[0].requires_approval);
    }

    #[test]
    fn test_deserialize_extracted_spans() {
        let json_data = r#"[
            {
                "title": "Hackathon Submission Prep",
                "summary": "Finalize demo recording and verify release gates",
                "start_time": "2026-09-28T14:00:00Z",
                "end_time": "2026-09-28T16:00:00Z",
                "category": "work"
            }
        ]"#;

        let spans: Vec<ExtractedSpan> = serde_json::from_str(json_data).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].category, "work");
    }
}
