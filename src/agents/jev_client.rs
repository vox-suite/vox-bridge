use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

pub const DEFAULT_JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
pub const DEFAULT_JEV_MODEL: &str = "jev-latest";

#[derive(Debug, thiserror::Error)]
pub enum JevError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error {0}: {1}")]
    Api(StatusCode, String),
    #[error("missing answer for question '{0}'")]
    MissingAnswer(String),
    #[error("unexpected answer type for question '{0}'")]
    UnexpectedType(String),
}

#[derive(Serialize)]
struct SystemOneRequest {
    state: Value,
    model: String,
    questions: HashMap<String, Question>,
}

#[derive(Serialize)]
struct Question {
    #[serde(rename = "type")]
    kind: String,
    instructions: String,
    criteria: HashMap<String, String>,
}

#[derive(Deserialize)]
struct SystemOneResponse {
    answers: HashMap<String, Answer>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Answer {
    Choice {
        choice: String,
        confidence: f64,
    },
    Noul {
        #[allow(dead_code)]
        noul: f64,
    },
    Score {
        #[allow(dead_code)]
        score: f64,
    },
}

#[derive(Clone)]
pub struct BridgeJevClient {
    http: Client,
    endpoint: String,
    api_key: String,
    model: String,
}

impl BridgeJevClient {
    pub fn new(api_key: String, endpoint: Option<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_millis(800))
            .tcp_nodelay(true)
            .build()
            .unwrap_or_default();
        Self {
            http,
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_JEV_URL.to_string()),
            api_key,
            model: DEFAULT_JEV_MODEL.to_string(),
        }
    }

    pub async fn choice(
        &self,
        state: Value,
        question_instructions: &str,
        options: &[(&str, Option<&str>)],
    ) -> Result<(String, f64), JevError> {
        let mut criteria = HashMap::new();
        for (k, v) in options {
            if let Some(desc) = v {
                criteria.insert(k.to_string(), desc.to_string());
            } else {
                criteria.insert(k.to_string(), k.to_string());
            }
        }

        let mut questions = HashMap::new();
        questions.insert(
            "decision".to_string(),
            Question {
                kind: "choice".to_string(),
                instructions: question_instructions.to_string(),
                criteria,
            },
        );

        let req = SystemOneRequest {
            state,
            model: self.model.clone(),
            questions,
        };

        let res = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(JevError::Api(status, body));
        }

        let resp: SystemOneResponse = res.json().await?;
        let ans = resp
            .answers
            .get("decision")
            .ok_or_else(|| JevError::MissingAnswer("decision".into()))?;

        match ans {
            Answer::Choice { choice, confidence } => Ok((choice.clone(), *confidence)),
            _ => Err(JevError::UnexpectedType("decision".into())),
        }
    }
}
