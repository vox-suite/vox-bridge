/**
* tests for channel durable tasks, proposals, narration, handoff, and uncertainty handling
*/
use axum::{Json, Router, extract::Path, http::StatusCode, routing::post};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::net::TcpListener;
use uuid::Uuid;

use vox_bridge::channels::context::CallContext;
use vox_bridge::channels::interaction::{
    ChannelCommand, ChannelInteractionError, ChannelInteractionHandler, HandoffNarrator,
    ProposalNarrator, StatusNarrator, UncertaintyNarrator, parse_channel_command,
};
use vox_bridge::core::client::CoreClient;
use vox_bridge::core::protocol::{
    ApproveRequestBody, ContextRequest, DurableTask, Proposal, RunState, WaitReason,
};

#[test]
fn test_parse_channel_commands() {
    let task_id = Uuid::new_v4();
    let proposal_id = Uuid::new_v4();

    assert_eq!(
        parse_channel_command("status"),
        ChannelCommand::Status { task_id: None }
    );
    assert_eq!(
        parse_channel_command(&format!("status {task_id}")),
        ChannelCommand::Status {
            task_id: Some(task_id)
        }
    );
    assert_eq!(
        parse_channel_command(&format!("approve {proposal_id}")),
        ChannelCommand::Approve { proposal_id }
    );
    assert_eq!(
        parse_channel_command(&format!("reject {proposal_id}")),
        ChannelCommand::Reject { proposal_id }
    );
    assert_eq!(
        parse_channel_command("clarify: I want to depart at 9 AM"),
        ChannelCommand::Clarify {
            task_id: None,
            text: "I want to depart at 9 AM".into()
        }
    );
    assert_eq!(
        parse_channel_command("clarification: Bangalore to London"),
        ChannelCommand::Clarify {
            task_id: None,
            text: "Bangalore to London".into()
        }
    );
    assert_eq!(
        parse_channel_command("Hello there!"),
        ChannelCommand::Text("Hello there!".into())
    );
}

#[test]
fn test_proposal_narrator_includes_material_details_and_disclaimers() {
    let proposal = Proposal {
        id: Uuid::new_v4(),
        capability_external_key: "travel.book_flight".into(),
        expires_at: "2026-09-24T12:00:00Z".into(),
        approval_id: None,
        details: json!({
            "provider": "Air India",
            "destination": "London Heathrow (LHR)",
            "price_minor": 4500000,
            "currency": "INR",
            "at": "2026-09-25 07:00 IST",
            "summary": "Non-stop economy flight BLR -> LHR"
        }),
    };

    let narration = ProposalNarrator::narrate(&proposal);

    assert!(narration.contains("travel.book_flight"));
    assert!(narration.contains("Air India"));
    assert!(narration.contains("London Heathrow (LHR)"));
    assert!(narration.contains("45000.00 INR"));
    assert!(narration.contains("2026-09-25 07:00 IST"));
    assert!(narration.contains("2026-09-24T12:00:00Z"));
    // Disclosures required by PRD and execution order
    assert!(narration.contains("explicit approval"));
    assert!(narration.contains("cannot execute this action independently"));
    assert!(narration.contains("Core coordinates execution"));
}

#[test]
fn test_handoff_narrator_explicitly_discloses_non_completion() {
    let task_id = Uuid::new_v4();
    let narration = HandoffNarrator::narrate(
        "Complex Hotel Booking",
        task_id,
        "Human Concierge Specialist",
        "Custom billing and special dietary requests",
    );

    assert!(narration.contains("Complex Hotel Booking"));
    assert!(narration.contains(&task_id.to_string()));
    assert!(narration.contains("Human Concierge Specialist"));
    assert!(narration.contains("Custom billing"));
    // Explicit disclosure required by E28
    assert!(narration.contains("NOT completed"));
    assert!(narration.contains("Bridge does not claim completion"));
}

#[test]
fn test_status_narrator_reports_accurate_authoritative_state() {
    let task_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    let queued_task = DurableTask {
        id: task_id,
        title: "Grocery Order".into(),
        state: RunState::Queued,
        run_id,
        wait_reason: None,
    };
    assert!(StatusNarrator::narrate(&queued_task).contains("queued and waiting to begin"));

    let running_task = DurableTask {
        id: task_id,
        title: "Grocery Order".into(),
        state: RunState::Running,
        run_id,
        wait_reason: None,
    };
    assert!(StatusNarrator::narrate(&running_task).contains("currently running"));

    let waiting_approval = DurableTask {
        id: task_id,
        title: "Grocery Order".into(),
        state: RunState::Waiting,
        run_id,
        wait_reason: Some(WaitReason::Approval),
    };
    assert!(
        StatusNarrator::narrate(&waiting_approval).contains("waiting for your explicit approval")
    );

    let waiting_clarification = DurableTask {
        id: task_id,
        title: "Grocery Order".into(),
        state: RunState::Waiting,
        run_id,
        wait_reason: Some(WaitReason::Clarification),
    };
    assert!(StatusNarrator::narrate(&waiting_clarification).contains("waiting for clarification"));

    let completed_task = DurableTask {
        id: task_id,
        title: "Grocery Order".into(),
        state: RunState::Completed,
        run_id,
        wait_reason: None,
    };
    assert!(StatusNarrator::narrate(&completed_task).contains("completed successfully"));

    let failed_task = DurableTask {
        id: task_id,
        title: "Grocery Order".into(),
        state: RunState::Failed,
        run_id,
        wait_reason: None,
    };
    assert!(StatusNarrator::narrate(&failed_task).contains("has failed"));
}

#[test]
fn test_uncertainty_narrator_emits_unconfirmed_and_partial_outcomes() {
    let unconfirmed =
        UncertaintyNarrator::unconfirmed_outcome("payment execution", "Core timeout after 30s");
    assert!(unconfirmed.contains("Unconfirmed Outcome"));
    assert!(unconfirmed.contains("authoritative run state remains unconfirmed"));
    assert!(unconfirmed.contains("No action was assumed completed"));
    assert!(unconfirmed.contains("app.voxagent.in"));

    let partial = UncertaintyNarrator::partial_success(
        "flight and hotel booking",
        "Flight BLR-DEL confirmed, hotel booking timed out",
    );
    assert!(partial.contains("Partial Outcome"));
    assert!(partial.contains("The task is not finished"));
}

#[tokio::test]
async fn test_sender_verification_blocks_unauthorized_parties() {
    let context = CallContext {
        channel: "whatsapp".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "whatsapp:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    // Valid sender
    assert!(ChannelInteractionHandler::verify_sender(&context, "+15551234567").is_ok());
    assert!(ChannelInteractionHandler::verify_sender(&context, "15551234567").is_ok());

    // Wrong sender trying to forge approval
    let err = ChannelInteractionHandler::verify_sender(&context, "+19998887777").unwrap_err();
    match err {
        ChannelInteractionError::UnauthorizedSender(sender) => {
            assert_eq!(sender, "+19998887777");
        }
        _ => panic!("Expected UnauthorizedSender error"),
    }
}

#[tokio::test]
async fn test_cannot_forge_approval_when_core_rejects_expired_or_changed_proposal() {
    let proposal_id = Uuid::new_v4();

    let app = Router::new().route(
        "/v1/action-proposals/{id}/approve",
        post(
            |Path(_id): Path<Uuid>, Json(_body): Json<ApproveRequestBody>| async {
                // Core returns 409 Conflict when proposal has expired or changed
                (
                    StatusCode::CONFLICT,
                    "proposal has expired or was superseded",
                )
            },
        ),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = CoreClient::new(format!("http://127.0.0.1:{port}"), "test-token".into()).unwrap();
    let context = CallContext {
        channel: "whatsapp".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "whatsapp:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    let result = ChannelInteractionHandler::handle_approve(
        &client,
        &context,
        "+15551234567",
        proposal_id,
        json!({"sku": "item-1"}),
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    match err {
        ChannelInteractionError::ProposalExpired(msg) => {
            assert!(msg.contains("expired or was superseded"));
        }
        other => panic!("Expected ProposalExpired, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_cannot_report_success_independently_when_core_is_unreachable() {
    let proposal_id = Uuid::new_v4();

    // Use a port where no server is listening
    let client = CoreClient::new("http://127.0.0.1:59999".into(), "test-token".into()).unwrap();
    let context = CallContext {
        channel: "whatsapp".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "whatsapp:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    let result = ChannelInteractionHandler::handle_approve(
        &client,
        &context,
        "+15551234567",
        proposal_id,
        json!({}),
    )
    .await;

    // Must fail closed with unconfirmed outcome, never forge success
    assert!(result.is_err());
    match result.unwrap_err() {
        ChannelInteractionError::CoreError(msg) => {
            assert!(msg.contains("Failed to approve action proposal"));
        }
        ChannelInteractionError::UnconfirmedOutcome(msg) => {
            assert!(msg.contains("Failed to approve action proposal"));
        }
        other => panic!("Expected CoreError/UnconfirmedOutcome, got: {other:?}"),
    }
}

#[tokio::test]
async fn test_reconnect_and_status_command_fetches_authoritative_run_state() {
    let task_id = Uuid::new_v4();
    let run_id = Uuid::new_v4();

    let query_count = Arc::new(AtomicUsize::new(0));
    let query_count_clone = query_count.clone();

    let app = Router::new().route(
        "/v1/durable-tasks/{id}",
        post(
            move |Path(id): Path<Uuid>, Json(_body): Json<ContextRequest>| {
                let count = query_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Json(DurableTask {
                        id,
                        title: "Book train tickets".into(),
                        state: RunState::Waiting,
                        run_id,
                        wait_reason: Some(WaitReason::Approval),
                    })
                }
            },
        ),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = CoreClient::new(format!("http://127.0.0.1:{port}"), "test-token".into()).unwrap();
    let context = CallContext {
        channel: "desktop".into(),
        external_identity: "user-host-123".into(),
        external_conversation_id: "desktop:session-456".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    // First query: on session start
    let status1 =
        ChannelInteractionHandler::handle_status(&client, &context, "user-host-123", task_id)
            .await
            .unwrap();

    assert!(status1.contains("Book train tickets"));
    assert!(status1.contains("waiting for your explicit approval"));

    // User disconnects and reconnects, issuing status query again
    let status2 =
        ChannelInteractionHandler::handle_status(&client, &context, "user-host-123", task_id)
            .await
            .unwrap();

    assert_eq!(status1, status2);
    assert_eq!(query_count.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_duplicate_decision_reports_duplicate_safely() {
    let proposal_id = Uuid::new_v4();

    let app = Router::new().route(
        "/v1/action-proposals/{id}/approve",
        post(
            |Path(_id): Path<Uuid>, Json(_body): Json<ApproveRequestBody>| async {
                (
                    StatusCode::CONFLICT,
                    "approval was already consumed for this proposal",
                )
            },
        ),
    );

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = CoreClient::new(format!("http://127.0.0.1:{port}"), "test-token".into()).unwrap();
    let context = CallContext {
        channel: "whatsapp".into(),
        external_identity: "+15551234567".into(),
        external_conversation_id: "whatsapp:+15551234567".into(),
        initiation_context: None,
        turn_id: None,
        revision: None,
        tts_provider: None,
        filler: None,
    };

    let result = ChannelInteractionHandler::handle_approve(
        &client,
        &context,
        "+15551234567",
        proposal_id,
        json!({}),
    )
    .await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ChannelInteractionError::DuplicateDecision(msg) => {
            assert!(msg.contains("already been processed or consumed"));
        }
        other => panic!("Expected DuplicateDecision, got: {other:?}"),
    }
}
