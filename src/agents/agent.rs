use rig::{
    client::AgentClientExt,
    completion::Prompt,
    providers::gemini::{self},
};

pub async fn gemini_agent_handler(user_query: &str) -> String {
    let gemini_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let client = gemini::Client::new(gemini_key).expect("Failed to create Gemini client");
    let agent = client
        .agent("gemini-3.5-flash-lite")
        .preamble("You are a helpful agent")
        .build();
    let response = agent.prompt(user_query).await;

    match response {
        Ok(msg) => msg,
        Err(e) => format!("Failed to communicate to LLM: {}", e),
    }
}
