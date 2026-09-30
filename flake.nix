{
  # inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
  inputs.parts.url = "github:hercules-ci/flake-parts";
  inputs.my-nix = { url = "github:nmrshll/nix-utils"; inputs.nixpkgs.follows = "nixpkgs"; inputs.fp.follows = "parts"; };
  # inputs.exo.url = "github:exo-explore/exo";

  # nixConfig = {
  #   extra-trusted-public-keys = "exo.cachix.org-1:okq7hl624TBeAR3kV+g39dUFSiaZgLRkLsFBCuJ2NZI=";
  #   extra-substituters = "https://exo.cachix.org";
  # };


  outputs = inputs@{ self, parts, my-nix, ... }: parts.lib.mkFlake { inherit inputs; } ({ lib, ... }:
    with builtins; {
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      imports = lib.flatten [
        (attrValues inputs.my-nix.flakeModules.essentials)
        inputs.my-nix.flakeModules.rust
        (inputs.my-nix.lib.findFlakePartFilesRec ./.)
      ];
      perSystem = { pkgs, config, lib, ... }:
        let
          # bin = inputs.my-nix.bin.${system} // (mapAttrs (n: p: "${p}/bin/${n}") scripts);
          # crane's `buildDepsOnly` builds the dependencies of every workspace
          # member, not just the one being packaged, so `nix build .#todo-2`
          # also has to satisfy demos/desktop-tauri's tauri stack on Linux:
          # gtk-sys, atk-sys, cairo-sys-rs, pango-sys, gdk-pixbuf-sys,
          # soup3-sys, webkit2gtk-sys and libdbus-sys each ask pkg-config for
          # their library at build time. macOS needs none of them: tauri and
          # tao use the system frameworks there.
          buildDeps = [
            pkgs.pkg-config
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.glib
            pkgs.gtk3
            pkgs.gdk-pixbuf
            pkgs.pango
            pkgs.cairo
            pkgs.atk
            pkgs.libsoup_3
            pkgs.webkitgtk_4_1
            pkgs.dbus
          ];

          # Node + pnpm supply the web design system under libs/ and its Astro
          # showcase under demos/ (see docs/spec/web-design-system-spec.md).
          devDeps = [ pkgs.cargo-tauri pkgs.cargo-watch pkgs.nodejs_22 pkgs.pnpm ];

          # The macOS bundle's Info.plist is checked in at
          # apps/todo-2/assets/Info.plist and its assembly recipe (icon,
          # signing, archives) lives in scripts/package-macos.sh, so the dev
          # bundle and the CI artifact are produced by one implementation.

          bash.wd = "$(git rev-parse --show-toplevel)";
          scripts = mapAttrs pkgs.writeShellScriptBin {
            # prun = ''set -x; package="$1"; shift; cargo run -p "$package" -- $@'';
            dt = ''set -e;  cd demos/desktop-tauri; cargo tauri dev '';
            ccheck = ''set -ex;
              cargo check -p desktop-gpui
              cargo check -p storage
              cargo check -p report_proc
              cargo check -p agent-cli
            '';
            dep-url = ''cargo metadata --format-version 1 2>/dev/null | \
                jq -r --arg dep "$1" \
                '.packages[] | select(.name == $dep) | .repository // empty' '';
            dep-url2 = ''curl -H "User-Agent: cargo-patch/0.1.0"  "https://crates.io/api/v1/crates/$1" | jq -r '.crate.repository' '';

            clone-patch = with bash; '' set -ex;
              DEP_NAME="$1"
              [ -d "${wd}/patched/$DEP_NAME" ] && echo "Error: ./patched/$DEP_NAME exists" && exit 1
              GIT_URL=$(cargo metadata --format-version 1 2>/dev/null | jq -r --arg d "$DEP_NAME" '.packages[] | select(.name == $d) | .repository // empty')
              echo "GIT_URL: $GIT_URL";
              [ -z "$GIT_URL" ] && GIT_URL=$(curl -s -H "User-Agent: cargo-patch/0.1.0"  "https://crates.io/api/v1/crates/$DEP_NAME" | jq -r '.crate.repository')
              [ -z "$GIT_URL" ] && printf "Error: no repository found for $DEP_NAME\n" && exit 1
              printf "%s\n" "Cloning $GIT_URL into ${wd}/patched/$DEP_NAME"
              git clone "$GIT_URL" "${wd}/patched/$DEP_NAME"
            '';

            mig = '' set -ex; cd libs/storage; cargo run --bin migrate -- migration "$@" '';
            testdbg = ''RUST_LOG=debug cargo test -p storage -- --nocapture --show-output'';

            # Dev loop: run the raw binary under cargo-watch so logs stream to
            # this terminal and file access keeps the shell's TCC identity
            # (a bundled launch prompts for Documents/Music/Photos instead).
            #
            # The watched paths are listed one by one, and passing any `-w`
            # turns off cargo-watch's own local-dependency discovery. Watching
            # the repository instead would let the pnpm/Astro side wake the
            # Rust build on every `node_modules`, `styled-system` or `dist`
            # write. These are the crates in todo-2's closure, at directory
            # granularity so a crate's embedded assets (`assets/icons`, reached
            # by `include_bytes!`) and its migrations (`toasty/migrations`,
            # reached by `include_dir!`) rebuild too — neither is a `.rs` file.
            # Add a line here when todo-2 gains an in-workspace dependency.
            t2 = ''cargo watch \
              -w apps/todo-2 \
              -w libs/storage \
              -w libs/acp-client \
              -w libs/gpui-tokio \
              -w libs/derive_entity_id \
              -w Cargo.toml \
              -w Cargo.lock \
              -x "run -p todo-2"'';

            # Assemble the macOS bundle for local use. The recipe (icon,
            # Info.plist, signing, de-quarantine) is scripts/package-macos.sh,
            # which the packaging job in .github/workflows/bundle.yml runs over a
            # release binary: `--link` keeps the bundle tracking rebuilds, and
            # `--register` is what gets the Dock to pick up a new icon.
            t2-bundle = with bash; ''set -e
              BIN_DIR="target/debug"
              if [ -n "''${CARGO_BUILD_TARGET:-}" ]; then
                BIN_DIR="target/''${CARGO_BUILD_TARGET}/debug"
              fi
              cargo build -p todo-2
              "${wd}/scripts/package-macos.sh" --binary "$BIN_DIR/todo-2" --out target/debug --link --register --no-archives
            '';

            # t2-clean-icon = ''rm -f target/debug/todo-lofi.app/Contents/Resources/todo-lofi.icns'';

            # Build test binaries, strip quarantine + sign them with the
            # self-signed todo-lofi-dev identity so Gatekeeper doesn't slow
            # down test launches, then run cargo test.
            tt = with bash; ''
              set -e
              TEST_BINS=$(cargo test "$@" --no-run --message-format=json 2>/dev/null | jq -r 'select(.reason == "compiler-artifact" and (.target.test // false)) | .filenames[]')
              if security find-identity -v -p codesigning 2>/dev/null | grep -q "todo-lofi-dev"; then
                for bin in $TEST_BINS; do
                  codesign --force --sign "todo-lofi-dev" "$bin" 2>/dev/null || true
                done
              fi
              for bin in $TEST_BINS; do
                xattr -d com.apple.quarantine "$bin" 2>/dev/null || true
              done
              cargo test "$@"
            '';

            skills = with bash; ''set -ex;
              for f in "${wd}"/docs/agent_skills/*.md; do
                [ -f "$f" ] || continue
                target="${wd}/.agents/skills/$(basename "$f" .md)/SKILL.md"
                mkdir -p "$(dirname "$target")";  cp "$f" "$target"
              done
            '';

            # The Astro CLI for the web packages. pnpm installs it per package
            # (node_modules/.bin), so it is not on PATH in the dev shell; this
            # runs the version the workspace pins rather than a nixpkgs "astro",
            # which is a different tool (uber/astro) entirely. Used for
            # `astro dev stop|status|logs`, `astro check`, and so on.
            astro = with bash; ''set -e
              for dir in "$PWD" "${wd}/apps/docs-web" "${wd}/demos/design-system-showcase"; do
                bin="$dir/node_modules/.bin/astro"
                if [ -x "$bin" ]; then exec "$bin" "$@"; fi
              done
              echo "astro: not installed yet - run 'pnpm install' first" >&2
              exit 1
            '';

            # Stop web servers left behind by a previous session. Astro records a
            # running `astro dev` / `astro preview` in the project's .astro/*.json
            # and refuses to start a second one, which is what `web` runs this for.
            # A stale preview server is the nastier case: the Playwright suites
            # reuse it and quietly test the previous build.
            dev-stop = with bash; ''set -e
              for dir in apps/docs-web demos/design-system-showcase; do
                [ -d "${wd}/$dir" ] || continue
                for cmd in dev preview; do
                  (cd "${wd}/$dir" && astro "$cmd" stop) || true
                done
              done
            '';

            # Docs site by default (`pnpm dev`); the design-system showcase is
            # `pnpm dev:ds`.
            web = ''set -e; pnpm install; dev-stop; pnpm dev'';
          };

          env = {
            # SNAFU_RAW_ERROR_MESSAGES = 1;
            # RUST_LIB_BACKTRACE = 1;
            # RUST_BACKTRACE = "1";
            RUST_LOG = "mini_gpui=debug,info"; # "toasty=debug";
          };

          # checks = {
          #   # Check that todo-core builds
          #   inherit todo-core;
          #   # Check that todo-core tests pass
          #   todo-core-tests = craneLib.cargoNextest (commonArgs // {
          #     inherit cargoArtifacts;
          #     packageFilter = [ "todo-core" ]; # Specify the package to test
          #     doCheck = true;
          #   });
          # };

        in
        let
          # Shared crane setup for the outputs below. my-nix's rust module
          # builds each crate from a fileset of just the crate's own
          # directory plus the workspace root manifests. That omits workspace
          # path dependencies (`todo-2 -> ../../libs/*`), so `cargo build -p
          # todo-2` fails in the sandbox with "failed to read
          # .../source/libs/acp-client/Cargo.toml". All outputs below build
          # from the full workspace source instead.
          crane = config.extraLib.craneLib;
          relPath = p: (/. + builtins.unsafeDiscardStringContext "${self.outPath + "${p}"}");
          # `commonCargoSources` drops non-Rust files, but todo-2 embeds its
          # icons via `include_bytes!` and storage embeds its migrations via
          # `include_dir!`, so keep those trees in the build source.
          fullSrc = lib.fileset.toSource {
            root = (/. + builtins.unsafeDiscardStringContext self.outPath);
            fileset = lib.fileset.unions [
              (crane.fileset.commonCargoSources (relPath "/"))
              (relPath "/apps/todo-2/assets")
              (relPath "/libs/storage/toasty")
            ];
          };
          fullDeps = crane.buildDepsOnly {
            src = fullSrc;
            buildInputs = config.rust.buildInputs;
            inherit (config.rust) nativeBuildInputs extraDummyScript;
            env = config.rust.buildEnv;
          };
        in
        {
          # packages = scripts;
          rust.buildInputs = buildDeps;
          rust.buildEnv = env;
          # crane's `buildDepsOnly` replaces every crate it finds with a dummy
          # stub. That includes the `patched/` cersei forks, but the git `cersei`
          # umbrella is a real dependency that links against the patched
          # `cersei-provider`, so the stub leaves `cersei_provider::{gemini,
          # openai, Auth, ...}` undefined and the deps build fails. Restore the
          # real patch sources once the dummy tree has been assembled.
          #
          # Needs my-nix to expose the crane `buildDepsOnly` escape hatches
          # (`rust.extraDummyScript`); run `nix flake update my-nix` for it.
          rust.extraDummyScript = ''
            rm -rf $out/patched
            cp -r --no-preserve=ownership ${self.outPath}/patched $out/patched
          '';
          # rust.toolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
          #   extensions = [ "rust-src" "rust-analyzer" ];
          #   targets = [ ];
          # });
          myDevShell.env = env;
          myDevShell.buildInputs = buildDeps ++ devDeps ++ (attrValues scripts);
          myDevShell.shellHooks = { };
          myDevShell.cleanups.icons.script = ''rm -f target/debug/todo-lofi.app/Contents/Resources/todo-lofi.icns'';

          # my-nix's `configure-editors` hook shells out to `code` unguarded,
          # so entering the devshell where VSCode is not installed prints
          # `code: command not found`. Only run it when `code` exists.
          myDevShell.shellHooks.configure-editors = lib.mkForce ''
            if command -v code >/dev/null 2>&1; then
              ${config.expose.packages.configure-editors}/bin/configure-editors
            fi
          '';

          # Hermetic test suites, run by CI via `nix build` instead of `nix
          # develop` + `cargo test`: the devshell is for local tools only, and
          # its incremental `target/` dir (restored from cache) is what
          # produced unreproducible sandbox failures like the missing rustls
          # build-script binary. Same crate set the `t2` watch loop covers.
          # Test binaries that commit to throwaway repos need a git identity,
          # hence GIT_* below (mirrors .github/workflows/test.yml).
          checks.todo-2-tests = crane.cargoTest {
            src = fullSrc;
            cargoArtifacts = fullDeps;
            nativeBuildInputs = config.rust.nativeBuildInputs ++ [
              pkgs.git
              # The coding-agent tests resolve the `opencode` binary from PATH
              # (the devshell carries it for the same reason).
              pkgs.opencode
            ];
            buildInputs = config.rust.buildInputs;
            pname = "todo-2-tests";
            version = "0.1.0";
            cargoTestExtraArgs = "-p todo-2 -p storage -p acp-client -p gpui_tokio";
            env = config.rust.buildEnv // {
              GIT_AUTHOR_NAME = "CI";
              GIT_AUTHOR_EMAIL = "ci@todo-lofi.invalid";
              GIT_COMMITTER_NAME = "CI";
              GIT_COMMITTER_EMAIL = "ci@todo-lofi.invalid";
              # Debug info for build scripts (e.g. rustls), so a failing
              # `cargoTest` phase reports usable backtraces.
              CARGO_PROFILE_TEST_BUILD_OVERRIDE_DEBUG = "true";
            };
          };

          # Linux binary of todo-2 built with one expression on any host:
          # native on a Linux host, cross-compiled on macOS. Rust cross needs
          # the target std plus a linker/C toolchain for the target; `zig cc`
          # provides both C/C++/ar behind wrapper scripts, so no per-host
          # pkgsCross stdenv is required. `doCheck` stays off: cross-built
          # test binaries cannot run on the build host; `checks.todo-2-tests`
          # runs the suites natively per runner instead.
          packages.todo-2-linux =
            let
              linuxTarget = "x86_64-unknown-linux-gnu";
              linuxTargetEnv = "x86_64_unknown_linux_gnu";
              linuxToolchain = config.expose.packages.customRust.override {
                targets = [ linuxTarget ];
              };
              linuxCrane = crane.overrideToolchain (_: linuxToolchain);
              # `zig cc` takes the target via `-target x86_64-linux-gnu`,
              # but cc-rs appends cargo's `--target=x86_64-unknown-linux-gnu`
              # (with the `unknown` vendor), which zig cannot parse, so filter
              # that flag out here.
              zigCc = pkgs.writeShellScriptBin "zig-cc-x86_64-linux" ''
                args=()
                for a in "$@"; do
                  case "$a" in --target=*) continue ;; *) args+=("$a") ;; esac
                done
                exec ${pkgs.zig}/bin/zig cc -target x86_64-linux-gnu "''${args[@]}"
              '';
              zigCxx = pkgs.writeShellScriptBin "zig-cxx-x86_64-linux" ''
                args=()
                for a in "$@"; do
                  case "$a" in --target=*) continue ;; *) args+=("$a") ;; esac
                done
                exec ${pkgs.zig}/bin/zig c++ -target x86_64-linux-gnu "''${args[@]}"
              '';
              crossEnv = {
                CARGO_BUILD_TARGET = linuxTarget;
                CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = "${zigCc}/bin/zig-cc-x86_64-linux";
                "CC_${linuxTargetEnv}" = "${zigCc}/bin/zig-cc-x86_64-linux";
                "CXX_${linuxTargetEnv}" = "${zigCxx}/bin/zig-cxx-x86_64-linux";
                "AR_${linuxTargetEnv}" = "${pkgs.llvmPackages.bintools}/bin/llvm-ar";
              };
            in
            # No `buildDepsOnly` layer here: it builds the dependencies of
            # every workspace member, including demos/desktop-tauri's Linux
            # gtk stack, which cannot resolve when cross-compiling from macOS.
            # Building `-p todo-2` directly only needs its own closure.
            linuxCrane.buildPackage {
              src = fullSrc;
              inherit (config.rust) nativeBuildInputs;
              buildInputs = config.rust.buildInputs;
              pname = "todo-2-linux";
              version = "0.1.0";
              cargoExtraArgs = "-p todo-2";
              doCheck = false;
              # `zig cc` resolves its cache dir via HOME, which the build env
              # leaves unset/unwritable; point it at the per-build temp dir.
              preBuild = ''export HOME="$TMPDIR"'';
              env = config.rust.buildEnv // crossEnv;
            };

          packages.todo-2 =
            lib.mkForce (crane.buildPackage {
              src = fullSrc;
              cargoArtifacts = fullDeps;
              inherit (config.rust) nativeBuildInputs;
              buildInputs = config.rust.buildInputs;
              pname = "todo-2";
              version = "0.1.0";
              cargoExtraArgs = "-p todo-2";
              doCheck = false;
              env = config.rust.buildEnv;
            });
        };
    });

}
