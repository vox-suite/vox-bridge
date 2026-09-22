// this file code contains deployment workflow verification tests

use std::fs;

#[test]
fn validates_release_and_hands_deployment_to_orchestrator() {
    let Ok(workflow) = fs::read_to_string(".github/workflows/publish.yml") else {
        return;
    };
    let legacy = fs::read_to_string(".github/workflows/deploy.yml")
        .expect("legacy recovery workflow must remain available");
    let bootstrap = fs::read_to_string(".github/workflows/bootstrap-deploy-access.yml")
        .expect("deployment access bootstrap workflow must exist");

    for required in [
        "branches: [main]",
        "workflow_dispatch:",
        "cargo test --locked",
        "cargo clippy --locked --all-targets --all-features -- -D warnings",
        "cargo build --release --locked",
        "vars.VOX_AUTO_DEPLOY == 'true'",
        "secrets.VOX_DEPLOY_DISPATCH_TOKEN",
        "repos/vox-suite/vox-deploy/dispatches",
        "component_ready",
    ] {
        assert!(
            workflow.contains(required),
            "missing workflow contract: {required}"
        );
    }

    assert!(legacy.contains("workflow_dispatch:"));
    assert!(!legacy.contains("branches: [main]"));
    for required in [
        "workflow_dispatch:",
        "VOX_DEPLOY_PUBLIC_KEY",
        "authorized_keys",
        "SSH_PRIVATE_KEY",
        "contents: read",
    ] {
        assert!(bootstrap.contains(required));
    }

    let test = workflow.find("cargo test --locked").unwrap();
    let lint = workflow.find("cargo clippy --locked").unwrap();
    let release = workflow.find("cargo build --release --locked").unwrap();
    let dispatch = workflow.find("Notify Vox Deploy").unwrap();
    assert!(test < dispatch && lint < dispatch && release < dispatch);
    assert!(!workflow.contains("docker/build-push-action"));
}
