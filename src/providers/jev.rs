use serde_json::json;

use std::time::Duration;

use crate::voice::provider::VoiceError;

#[derive(Clone)]
pub struct JevClient {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
}

impl JevClient {
    pub fn new(http: reqwest::Client, api_key: String) -> Self {
        Self {
            http,
            api_key,
            endpoint: "https://api.typesafe.ai/v1/systemone".to_string(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = endpoint;
        self
    }

    async fn post_system_one(
        &self,
        body: &serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, VoiceError> {
        let response = self
            .http
            .post(&self.endpoint)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .timeout(timeout)
            .json(body)
            .send()
            .await
            .map_err(|err| VoiceError::Provider {
                provider: "jev",
                message: err.to_string(),
            })?;

        if !response.status().is_success() {
            return Err(VoiceError::Provider {
                provider: "jev",
                message: format!("HTTP {}", response.status()),
            });
        }

        let parsed: serde_json::Value =
            response.json().await.map_err(|err| VoiceError::Provider {
                provider: "jev",
                message: format!("failed to parse Jev response: {err}"),
            })?;

        Ok(parsed)
    }

    pub async fn choose_acknowledgement(
        &self,
        transcript: &str,
    ) -> Result<Option<&'static str>, VoiceError> {
        let mut criteria = serde_json::Map::new();
        criteria.insert("none".into(), json!("No acknowledgment for greetings, gratitude, closures, or questions that need no lookup or work."));
        for (index, phrase) in crate::voice::filler::ACKNOWLEDGEMENTS.iter().enumerate() {
            criteria.insert(index.to_string(), json!(phrase));
        }
        let body = json!({
            "state": transcript,
            "model": "jev-latest",
            "questions": {
                "acknowledgement": {
                    "type": "choice",
                    "instructions": "Choose one brief, natural acknowledgment to speak while the main assistant works in parallel. Match the request and its tone. Weather phrases only for weather requests; order and account phrases only for those requests. Never claim a result, success, or completed action. Choose none if no lookup or work is needed.",
                    "criteria": criteria
                }
            }
        });
        let parsed = self
            .post_system_one(&body, Duration::from_millis(500))
            .await?;
        let choice = parsed["answers"]["acknowledgement"]["choice"]
            .as_str()
            .ok_or_else(|| VoiceError::Provider {
                provider: "jev",
                message: "invalid acknowledgement choice".into(),
            })?;
        if choice == "none" {
            return Ok(None);
        }
        let phrase = choice
            .parse::<usize>()
            .ok()
            .and_then(|index| crate::voice::filler::ACKNOWLEDGEMENTS.get(index))
            .copied()
            .ok_or_else(|| VoiceError::Provider {
                provider: "jev",
                message: "unknown acknowledgement choice".into(),
            })?;
        Ok(Some(phrase))
    }

    pub async fn is_complete_thought(&self, transcript: &str) -> Result<f64, VoiceError> {
        let body = json!(
            {
                "state": transcript,
                "model": "jev-latest",
                "questions": {
                    "is_complete": {
                        "type": "noul",
                        "instructions": "Is this customer utterance a grammatically complete thought, statement, or question, rather than a sentence cut off mid-thought?",
                        "criteria": {
                            "true": "A complete sentence, question, command, or finalized answer.",
                            "false": "Sentence ends abruptly with a conjunction, preposition, trailing thought, or incomplete clause."
                        }
                    }
                }
            }
        );

        let parsed = self
            .post_system_one(&body, Duration::from_millis(300))
            .await?;
        let prob = parsed["answers"]["is_complete"]["noul"]
            .as_f64()
            .unwrap_or(0.5);

        Ok(prob)
    }
}
