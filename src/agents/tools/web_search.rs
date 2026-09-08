use reqwest::Client;
use rig::tool::Tool;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct SearchArgs {
    pub query: String,
}

pub struct WebSearch {
    client: Client,
    searxng_url: String,
}

impl WebSearch {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            searxng_url: std::env::var("SEARXNG_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned()),
        }
    }
}

impl Tool for WebSearch {
    const NAME: &'static str = "web_search";
    type Args = SearchArgs;
    type Output = String;
    type Error = reqwest::Error;

    fn description(&self) -> String {
        "Search the given query in the internet".to_owned()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                }
            },
            "required": ["query"]
        })
    }
    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let started_at = std::time::Instant::now();
        let endpoint = format!("{}/search", self.searxng_url.trim_end_matches('/'));

        eprintln!(
            "agent_tool event=start tool=web_search endpoint={} query_chars={}",
            endpoint,
            args.query.chars().count()
        );

        let response = match self
            .client
            .get(&endpoint)
            .query(&[("q", args.query.as_str()), ("format", "json")])
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                eprintln!(
                    "agent_tool event=request_failed tool=web_search elapsed_ms={} error={}",
                    started_at.elapsed().as_millis(),
                    error
                );
                return Err(error);
            }
        };

        eprintln!(
            "agent_tool event=response tool=web_search status={} elapsed_ms={}",
            response.status(),
            started_at.elapsed().as_millis()
        );

        let response = response.error_for_status()?.text().await?;

        eprintln!(
            "agent_tool event=complete tool=web_search elapsed_ms={} response_bytes={}",
            started_at.elapsed().as_millis(),
            response.len()
        );

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_searxng_url_targets_local_port_8080() {
        let tool = WebSearch::new(Client::new());

        assert_eq!(tool.searxng_url, "http://127.0.0.1:8080");
    }
}
