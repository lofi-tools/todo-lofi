//! Keep the v2 GPUI shell unstarted until v1 is feature complete.
//!
//! The planned v2 shell lives in `apps/shepherd-gpui` and reuses
//! `libs/gpui-tokio`; neither belongs in the Shepherd tree while v1 is still
//! open. Taskstream's separate GPUI crates are outside this rule.

use std::fs;
use std::path::{Path, PathBuf};

const SHEPHERD_MANIFESTS: [&str; 4] = [
    "apps/shepherd/src-tauri/Cargo.toml",
    "libs/shepherd-core/Cargo.toml",
    "libs/agent-harness/Cargo.toml",
    "libs/acp-client/Cargo.toml",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
fn v2_gpui_shell_is_not_started() -> Result<(), Box<dyn std::error::Error>> {
    let root = repo_root();
    assert!(
        !root.join("apps/shepherd-gpui").exists(),
        "v2 shell scaffolding must not exist before v1 is feature complete"
    );

    for manifest in SHEPHERD_MANIFESTS {
        let path = root.join(manifest);
        let contents = fs::read_to_string(&path)?;
        for (index, line) in contents.lines().enumerate() {
            let dependency = line.split('#').next().unwrap_or_default();
            assert!(
                !dependency.contains("gpui"),
                "{}:{} adds a GPUI dependency before v1 is feature complete: {line}",
                path.display(),
                index + 1,
            );
        }
    }

    let spec = fs::read_to_string(root.join("apps/shepherd/docs/spec/master-spec.md"))?;
    assert!(
        spec.contains("work starts when v1 is feature complete"),
        "the spec must keep the v1 gate on v2"
    );
    let stack = fs::read_to_string(root.join("apps/shepherd/docs/spec/stack.md"))?;
    assert!(
        stack.contains("## v2 shell (GPUI)"),
        "the v2 plan must stay documented"
    );
    Ok(())
}
