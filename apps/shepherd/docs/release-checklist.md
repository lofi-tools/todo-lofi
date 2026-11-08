# Shepherd release checklist

Status: **not yet run**. This is the clean-machine gate for a v1 release; the
release is not ready until both platform rows in [Evidence](#evidence) are
filled from a clean machine at the release commit.

`cargo test -p shepherd --test packaging` keeps this document, the Tauri
configuration, the app build script and the package smoke workflow in
agreement. The manually dispatched `shepherd-package-smoke` workflow is
preflight only; it does not replace a clean-machine run.

## Preflight (any machine)

Run from the repository root inside the Nix dev shell (`direnv allow .` once).

- [ ] `direnv exec . cargo fmt --check`
- [ ] `direnv exec . cargo clippy --workspace -- -D warnings`
- [ ] `direnv exec . cargo test -p agent-harness`
- [ ] `direnv exec . cargo test -p shepherd-core`
- [ ] `direnv exec . cargo test -p shepherd`
- [ ] `direnv exec . cargo test -p shepherd --test packaging`
- [ ] `direnv exec . cargo test -p shepherd --test shell_scope`
- [ ] `direnv exec . pnpm --dir apps/shepherd/ui run typecheck`
- [ ] `direnv exec . pnpm --dir apps/shepherd/ui run test:e2e`
- [ ] Record the commit with `git rev-parse HEAD`; it must be the commit being
  released.

## macOS 13+ (Ventura or newer)

Use a clean macOS 13+ machine or VM with the Xcode command-line tools and the
Nix daemon; do not reuse a developer `target/` or Nix store.

- [ ] `pnpm install --frozen-lockfile`
- [ ] `rm -rf target`, or set a fresh `CARGO_TARGET_DIR`
- [ ] `direnv exec . pnpm --dir apps/shepherd build --bundles app`
- [ ] Confirm the bundle exists at
  `target/release/bundle/macos/Shepherd.app`
- [ ] `plutil -p target/release/bundle/macos/Shepherd.app/Contents/Info.plist | grep LSMinimumSystemVersion`
  reports `13.0`
- [ ] `xcrun vtool -show-build target/release/bundle/macos/Shepherd.app/Contents/MacOS/shepherd | grep minos`
  reports `minos 13.0`
- [ ] `otool -L target/release/bundle/macos/Shepherd.app/Contents/MacOS/shepherd`
  has no `/nix/store/` entry; if it does, the bundle is not portable and the
  release stops.
- [ ] Launch the bundle on macOS 13, add a disposable Git repository, start a
  Codex task, answer an approval, inspect the diff, quit with the agent
  running, relaunch, and resume.
- [ ] `direnv exec . ./scripts/smoke-shepherd-terminal.sh --macos-window`
  (needs `python3`, `vim`, `less` and `fzf`; keep the window visible).
- [ ] Record signing status (these builds are unsigned unless separately
  signed) and any Gatekeeper prompt in Evidence.

## Ubuntu 24.04+

Use a clean Ubuntu 24.04 x86-64 or arm64 machine or VM with the Nix daemon and
the smoke fixtures.

- [ ] `pnpm install --frozen-lockfile`
- [ ] `rm -rf target`
- [ ] `direnv exec . pnpm --dir apps/shepherd build --bundles deb`
- [ ] Confirm exactly one package with
  `find target/release/bundle/deb -name '*.deb'`
- [ ] `dpkg-deb --info <package>`
- [ ] `dpkg-deb --contents <package> | grep -F 'usr/bin/shepherd'`
- [ ] Extract the package and run `readelf -dW <path>/usr/bin/shepherd`; no
  `/nix/store/` path may appear, or the package is not portable.
- [ ] Install with `sudo dpkg -i <package>` (or `apt install ./<package>`) and
  launch it under the desktop session; confirm WebKitGTK 2.44 or newer.
- [ ] Add a repository, start a Codex task, answer an approval and inspect the
  diff.
- [ ] `direnv exec . ./scripts/smoke-shepherd-terminal.sh` under the desktop
  session or Xvfb; `--linux-ime` additionally needs IBus and Pinyin.
- [ ] Record the result in Evidence.

## Human-gated steps

These cannot be completed by the offline test suite or on the developer
machine.

- [ ] Clean macOS 13+ and Ubuntu 24.04+ machines are available for the runs
  above.
- [ ] Codex is installed and signed in; OpenCode, Pi and Claude Code are
  installed with their credentials where their smokes are claimed.
- [ ] `settings.json` contains the installed-agent paths and any `!command`
  environment values; never paste secret values into this checklist or logs.
- [ ] Release signing and notarization are either configured by the release
  owner or the unsigned status is accepted for this release.

## Evidence

| Platform | Commit | Date | Machine | Result | Operator |
| --- | --- | --- | --- | --- | --- |
| macOS 13+ | | | | Pending — human-gated | |
| Ubuntu 24.04+ | | | | Pending — human-gated | |

Local preflight evidence, not a clean-machine release run: on 2026-11-08 the
debug macOS bundle built for the packaging item reported
`LSMinimumSystemVersion 13.0` and `minos 13.0` on macOS 15.7.4. That proves the
build settings, not the release checklist.
