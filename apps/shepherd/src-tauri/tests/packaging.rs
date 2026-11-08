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
