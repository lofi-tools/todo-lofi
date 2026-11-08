//! Keep the live adapter smoke workflow opt-in and fail closed.
//!
//! The live tests need installed third-party agents and runner-local settings.
//! Their feature gates keep them out of the default suite; the workflow's
//! configuration gate must reject a dispatch before a test can pass without
//! the integration present.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn read(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

#[test]
fn adapter_smoke_workflow_is_opt_in_and_covers_each_integration()
-> Result<(), Box<dyn std::error::Error>> {
    let root = repo_root();
    let workflow = read(&root.join(".github/workflows/shepherd-adapter-smoke.yml"))?;
    for required in [
        "workflow_dispatch:",
        "- opencode",
        "- pi",
        "- claude-acp",
        "refs/heads/{0}",
        "runs-on: [self-hosted, shepherd-adapters]",
        "check-adapter-smoke-config.sh",
        "SHEPHERD_ADAPTER_TEST_SETTINGS",
        "SHEPHERD_PI_TEST_EXECUTABLE",
        "--features live-opencode",
        "--features live-pi",
        "--features live-claude",
        "opencode::tests::installed_",
        "pi::tests::installed_pi_exchanges_extension_dialog_records",
        "acp::tests::installed_claude_code_opens_an_acp_session",
    ] {
        assert!(
            workflow.contains(required),
            "the adapter smoke workflow is missing {required}"
        );
    }

    let procedure = read(&root.join("apps/shepherd/docs/adapter-live-smoke.md"))?;
    for required in [
        "SHEPHERD_OPENCODE_TEST_SETTINGS",
        "SHEPHERD_PI_TEST_EXECUTABLE",
        "SHEPHERD_CLAUDE_TEST_SETTINGS",
        "Pending — human-gated",
        "cargo test -p agent-harness --features live-opencode",
        "cargo test -p agent-harness --features live-pi",
        "cargo test -p agent-harness --features live-claude",
    ] {
        assert!(
            procedure.contains(required),
            "the adapter smoke procedure is missing {required}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn adapter_smoke_gate_rejects_missing_configuration() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let root = repo_root();
    let script = root.join("scripts/check-adapter-smoke-config.sh");
    let run = |agent: &str| {
        Command::new("bash")
            .arg(&script)
            .arg(agent)
            .env_remove("SHEPHERD_ADAPTER_TEST_SETTINGS")
            .env_remove("SHEPHERD_PI_TEST_EXECUTABLE")
            .output()
    };

    for (agent, variable) in [
        ("opencode", "SHEPHERD_ADAPTER_TEST_SETTINGS"),
        ("claude-acp", "SHEPHERD_ADAPTER_TEST_SETTINGS"),
        ("pi", "SHEPHERD_PI_TEST_EXECUTABLE"),
    ] {
        let output = run(agent)?;
        assert!(
            !output.status.success(),
            "the {agent} smoke gate passed without {variable}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(variable),
            "the {agent} smoke gate did not name {variable}"
        );
    }

    let unknown = run("codex")?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("unknown adapter smoke"),
        "an unknown adapter must be rejected explicitly"
    );

    let directory =
        std::env::temp_dir().join(format!("shepherd-adapter-smoke-{}", std::process::id()));
    fs::create_dir_all(&directory)?;
    let settings = directory.join("settings.json");
    fs::write(&settings, "{}")?;
    let configured = Command::new("bash")
        .arg(&script)
        .arg("opencode")
        .env("SHEPHERD_ADAPTER_TEST_SETTINGS", &settings)
        .env_remove("SHEPHERD_PI_TEST_EXECUTABLE")
        .output()?;
    assert!(
        configured.status.success(),
        "a readable settings path must pass the gate: {}",
        String::from_utf8_lossy(&configured.stderr)
    );

    let executable = directory.join("pi");
    fs::write(&executable, "#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    let configured = Command::new("bash")
        .arg(&script)
        .arg("pi")
        .env("SHEPHERD_PI_TEST_EXECUTABLE", &executable)
        .env_remove("SHEPHERD_ADAPTER_TEST_SETTINGS")
        .output()?;
    assert!(
        configured.status.success(),
        "an executable Pi path must pass the gate: {}",
        String::from_utf8_lossy(&configured.stderr)
    );

    fs::remove_dir_all(directory)?;
    Ok(())
}
