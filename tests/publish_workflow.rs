use std::fs;

#[test]
fn publishes_tested_arm64_image_and_hands_deployment_to_orchestrator() {
    let workflow = fs::read_to_string(".github/workflows/publish.yml")
        .expect("Bridge publication workflow must exist");
    let legacy = fs::read_to_string(".github/workflows/deploy.yml")
        .expect("legacy recovery workflow must remain available");
    let bootstrap = fs::read_to_string(".github/workflows/bootstrap-deploy-access.yml")
        .expect("deployment access bootstrap workflow must exist");

    for required in [
        "branches: [main]",
        "workflow_dispatch:",
        "packages: write",
        "cargo test --locked",
        "cargo clippy --locked --all-targets --all-features -- -D warnings",
        "cargo build --release --locked",
        "docker/login-action@v4",
        "docker/setup-buildx-action@v4",
        "docker/build-push-action@v7",
        "platforms: linux/arm64",
        "ghcr.io/vox-suite/vox-bridge:${{ github.sha }}",
        "digest: ${{ steps.build.outputs.digest }}",
        "vars.VOX_AUTO_DEPLOY == 'true'",
        "secrets.VOX_DEPLOY_DISPATCH_TOKEN",
        "repos/vox-suite/vox-deploy/dispatches",
        "component_published",
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
    let publish = workflow.find("docker/build-push-action@v7").unwrap();
    assert!(test < publish && lint < publish && release < publish);
}
