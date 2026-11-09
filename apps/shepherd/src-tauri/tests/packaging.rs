//! Keep the packaging versions and build path in one place.
//!
//! The deployment target and toolchain facts live in the Tauri config, the app
//! build script, the CI workflows and the Nix flake; this test fails when they
//! drift apart.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn read(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
        .map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

#[test]
fn packaging_versions_and_build_path_agree() -> Result<(), Box<dyn std::error::Error>> {
    let root = repo_root();
    let app = root.join("apps/shepherd");

    let tauri: serde_json::Value =
        serde_json::from_str(&read(&app.join("src-tauri/tauri.conf.json"))?)?;
    assert_eq!(tauri["productName"], "Shepherd");
    assert_eq!(tauri["identifier"], "dev.shepherd.shell");
    assert_eq!(tauri["bundle"]["macOS"]["minimumSystemVersion"], "13.0");

    let package: serde_json::Value = serde_json::from_str(&read(&app.join("package.json"))?)?;
    assert_eq!(package["version"], tauri["version"]);
    let build = package["scripts"]["build"]
        .as_str()
        .ok_or_else(|| io::Error::other("apps/shepherd/package.json has no build script"))?;
    assert!(
        build.contains("MACOSX_DEPLOYMENT_TARGET=13.0"),
        "the app build must target macOS 13.0: {build}"
    );
    assert!(
        build.contains("tauri build"),
        "the app build must go through the Tauri CLI: {build}"
    );

    let smoke = read(&root.join(".github/workflows/shepherd-package-smoke.yml"))?;
    assert!(smoke.contains("runs-on: macos-14"), "missing macOS runner");
    assert!(
        smoke.contains("runs-on: ubuntu-24.04"),
        "missing Ubuntu 24.04 runner"
    );
    assert!(
        smoke.contains("LSMinimumSystemVersion"),
        "the smoke must read the bundle's deployment target"
    );
    assert!(
        smoke.contains("minos 13\\.0"),
        "the smoke must check the executable's minimum macOS version"
    );
    assert!(
        smoke.contains("dpkg-deb --info"),
        "the smoke must inspect the Debian package"
    );
    assert!(
        smoke.contains("/nix/store/"),
        "the smoke must reject packaged Nix store dependencies"
    );
    assert!(
        smoke.contains("cargo test -p shepherd --test packaging"),
        "the smoke must run the packaging consistency test"
    );

    let flake = read(&root.join("flake.nix"))?;
    assert!(flake.contains("pkgs.zig"), "the flake must provide Zig");
    assert!(
        root.join("flake.lock").is_file(),
        "the flake lock must pin the Zig toolchain"
    );

    let readme = read(&app.join("README.md"))?;
    assert!(
        readme.contains("macOS deployment target to 13.0"),
        "the README must document the deployment target"
    );
    Ok(())
}

#[test]
fn release_checklist_tracks_the_build_and_ci() -> Result<(), Box<dyn std::error::Error>> {
    let root = repo_root();
    let checklist = read(&root.join("apps/shepherd/docs/release-checklist.md"))?;
    for section in [
        "## Preflight",
        "## macOS 13+",
        "## Ubuntu 24.04+",
        "## Human-gated steps",
        "## Evidence",
    ] {
        assert!(
            checklist.contains(section),
            "the release checklist is missing {section}"
        );
    }
    for command in [
        "cargo fmt --check",
        "cargo clippy --workspace -- -D warnings",
        "cargo test -p shepherd --test packaging",
        "cargo test -p shepherd --test shell_scope",
        "pnpm --dir apps/shepherd build --bundles app",
        "target/release/bundle/macos/Shepherd.app",
        "LSMinimumSystemVersion",
        "xcrun vtool -show-build",
        "pnpm --dir apps/shepherd build --bundles deb",
        "target/release/bundle/deb",
        "dpkg-deb --info",
        "readelf -dW",
        "./scripts/smoke-shepherd-terminal.sh --macos-window",
        "Pending — human-gated",
    ] {
        assert!(
            checklist.contains(command),
            "the release checklist is missing {command}"
        );
    }

    let readme = read(&root.join("apps/shepherd/README.md"))?;
    assert!(
        readme.contains("docs/release-checklist.md"),
        "the README must link the release checklist"
    );
    Ok(())
}

#[test]
fn release_matrix_builds_every_target_and_verifies_before_publishing()
-> Result<(), Box<dyn std::error::Error>> {
    let root = repo_root();
    let workflow = read(&root.join(".github/workflows/shepherd-release.yml"))?;
    assert!(
        workflow.contains("tags: [\"shepherd-v*\"]"),
        "the release must run from a shepherd-v* tag"
    );
    for runner in [
        "macos-14",
        "macos-15-intel",
        "ubuntu-24.04",
        "ubuntu-24.04-arm",
        "windows-2025",
    ] {
        assert!(
            workflow.contains(&format!("os: {runner}")),
            "the matrix is missing {runner}"
        );
    }
    for arch in ["arch: arm64", "arch: x86_64"] {
        assert!(workflow.contains(arch), "the matrix is missing {arch}");
    }
    assert!(
        workflow.contains("needs: [test, build]"),
        "publishing must wait for the test workflow and every bundle"
    );
    assert!(
        workflow.contains("startsWith(github.ref, 'refs/tags/shepherd-v')"),
        "publishing must be restricted to Shepherd release tags"
    );
    assert!(
        workflow.contains("scripts/shepherd-release-metadata.sh"),
        "the workflow must record release metadata"
    );
    assert!(
        workflow.contains("--check dist"),
        "publishing must re-verify the recorded checksums"
    );
    assert!(
        workflow.contains("gh release create"),
        "the workflow must publish the release"
    );
    Ok(())
}

#[test]
fn release_metadata_records_checksums_and_rejects_a_missing_bundle()
-> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};

    use std::process::Command;

    let root = repo_root();
    let script = root.join("scripts/shepherd-release-metadata.sh");
    let base =
        std::env::temp_dir().join(format!("shepherd-release-metadata-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let dist = base.join("dist");
    fs::create_dir_all(&dist)?;
    let zip = dist.join("shepherd-0.1.0-macos-arm64.zip");
    let dmg = dist.join("shepherd-0.1.0-macos-arm64.dmg");
    fs::write(&zip, b"zip bytes")?;
    fs::write(&dmg, b"dmg bytes")?;

    let metadata_path = |dist: &Path, name: &str| dist.join(name);
    let record = |out: &Path| -> Result<std::process::Output, io::Error> {
        Command::new("bash")
            .arg(&script)
            .arg("--dist")
            .arg(&dist)
            .args([
                "--version",
                "0.1.0",
                "--commit",
                "abc123",
                "--platform",
                "macos",
                "--arch",
                "arm64",
                "--expect",
                "zip",
                "--expect",
                "dmg",
                "--out",
            ])
            .arg(out)
            .output()
    };
    let out = metadata_path(&dist, "release-metadata-macos-arm64.json");
    let output = record(&out)?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_str(&fs::read_to_string(&out)?)?;
    assert_eq!(metadata["version"], "0.1.0");
    assert_eq!(metadata["commit"], "abc123");
    assert_eq!(metadata["platform"], "macos");
    assert_eq!(metadata["arch"], "arm64");
    let digest = |bytes: &[u8]| {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let artifacts = metadata["artifacts"]
        .as_array()
        .ok_or("artifacts must be a list")?;
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0]["name"], "shepherd-0.1.0-macos-arm64.dmg");
    assert_eq!(artifacts[0]["sha256"], digest(b"dmg bytes"));
    assert_eq!(artifacts[0]["bytes"], 9);
    assert_eq!(artifacts[1]["name"], "shepherd-0.1.0-macos-arm64.zip");
    assert_eq!(artifacts[1]["sha256"], digest(b"zip bytes"));

    for target in [
        "macos-x86_64",
        "linux-x86_64",
        "linux-arm64",
        "windows-x86_64",
    ] {
        let out = metadata_path(&dist, &format!("release-metadata-{target}.json"));
        let output = record(&out)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let check = || {
        Command::new("bash")
            .arg(&script)
            .arg("--check")
            .arg(&dist)
            .args(["--version", "0.1.0", "--commit", "abc123"])
            .output()
    };
    let verified = check()?;
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    fs::write(&zip, b"changed")?;
    let stale = check()?;
    assert!(!stale.status.success());
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("checksum mismatch"),
        "a changed artifact must fail verification"
    );

    let empty = base.join("empty");
    fs::create_dir_all(&empty)?;
    let failed = Command::new("bash")
        .arg(&script)
        .arg("--dist")
        .arg(&empty)
        .args([
            "--version",
            "0.1.0",
            "--commit",
            "abc123",
            "--platform",
            "macos",
            "--arch",
            "arm64",
            "--expect",
            "zip",
            "--out",
        ])
        .arg(empty.join("release-metadata-macos-arm64.json"))
        .output()?;
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("no artifacts"),
        "an empty target must fail instead of recording nothing"
    );

    let _ = fs::remove_dir_all(&base);
    Ok(())
}
