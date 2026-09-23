/**
* this file code contains speculative core agent execution
*/
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::channels::context::CallContext;
use crate::core::ConversationClient;

pub fn spawn_speculation_watcher(
    mut speculation_rx: watch::Receiver<Option<(CallContext, String)>>,
    agent: Arc<dyn ConversationClient>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while speculation_rx.changed().await.is_ok() {
            loop {
                tokio::select! {
                    result = speculation_rx.changed() => { if result.is_err() { return; } }
                    _ = tokio::time::sleep(Duration::from_millis(250)) => break,
                }
            }
            let snapshot = speculation_rx.borrow_and_update().clone();
            if let Some((ctx, text)) = snapshot {
                tokio::select! {
                    result = agent.speculate(&ctx, &text) => {
                        if let Err(err) = result {
                            tracing::debug!(error = %err, "VOICE_SPECULATION_UNAVAILABLE");
                        }
                    }
                    result = speculation_rx.changed() => {
                        if result.is_err() { return; }
                        speculation_rx.mark_changed();
                    }
                }
            }
        }
    })
}
