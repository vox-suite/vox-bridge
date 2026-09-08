use rig::{
    client::AgentClientExt,
    completion::Prompt,
    providers::gemini::{self},
};

pub async fn gemini_agent_handler(user_query: &str) -> String {
    eprintln!(
        "agent event=start provider=gemini prompt_chars={}",
        user_query.chars().count()
    );

    let tool_dep = crate::agents::tools::tool_dependencies::ToolDependencies::new()
        .expect("http client create failed");

    let web_search = crate::agents::tools::web_search::WebSearch::new(tool_dep.http.clone());

    let gemini_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let client = gemini::Client::new(gemini_key).expect("Failed to create Gemini client");
    let agent = client
        .agent("gemini-3.5-flash-lite")
        .preamble("You are a helpful agent")
        .tool(web_search)
        .default_max_turns(3)
        .build();
    let response = agent.prompt(user_query).await;

    match response {
        Ok(msg) => {
            eprintln!(
                "agent event=complete provider=gemini response_chars={}",
                msg.chars().count()
            );
            msg
        }
        Err(e) => {
            eprintln!("agent event=failed provider=gemini error={}", e);
            format!("Failed to communicate to LLM: {}", e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "calls the live Gemini API and configured tools"]
    async fn debug_live_agent() {
        let query = std::env::var("VOX_TEST_QUERY")
            .unwrap_or_else(|_| "What is the latest stable Rust version?".to_owned());

        let response = gemini_agent_handler(&query).await;
        eprintln!("agent_debug response={response}");
    }
}
