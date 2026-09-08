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
            searxng_url: "http://127.0.0.1/8080/search".to_owned(),
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
        println!("web search called");
        let response = self
            .client
            .get(format!("{}/search", self.searxng_url.trim_end_matches('/')))
            .query(&[("q", args.query.as_str()), ("format", "json")])
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        Ok(response)
    }
}
