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
          # member, not just the one being packaged, so `nix build .#taskstream-desktop`
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
          # apps/taskstream-desktop/assets/Info.plist and its assembly recipe (icon,
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
            # write. These are the crates in taskstream-desktop's closure, at directory
            # granularity so a crate's embedded assets (`assets/icons`, reached
            # by `include_bytes!`) and its migrations (`toasty/migrations`,
            # reached by `include_dir!`) rebuild too — neither is a `.rs` file.
            # Add a line here when taskstream-desktop gains an in-workspace dependency.
            t2 = ''cargo watch \
              -w apps/taskstream-desktop \
              -w libs/storage \
              -w libs/acp-client \
              -w libs/gpui-tokio \
              -w libs/derive_entity_id \
              -w Cargo.toml \
              -w Cargo.lock \
              -x "run -p taskstream-desktop"'';

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
              cargo build -p taskstream-desktop
              "${wd}/scripts/package-macos.sh" --binary "$BIN_DIR/taskstream-desktop" --out target/debug --link --register --no-archives
            '';

            # t2-clean-icon = ''rm -f target/debug/taskstream.app/Contents/Resources/taskstream.icns'';

            # Build test binaries, strip quarantine + sign them with the
            # self-signed taskstream-dev identity so Gatekeeper doesn't slow
            # down test launches, then run cargo test.
            tt = with bash; ''
              set -e
              TEST_BINS=$(cargo test "$@" --no-run --message-format=json 2>/dev/null | jq -r 'select(.reason == "compiler-artifact" and (.target.test // false)) | .filenames[]')
              if security find-identity -v -p codesigning 2>/dev/null | grep -q "taskstream-dev"; then
                for bin in $TEST_BINS; do
                  codesign --force --sign "taskstream-dev" "$bin" 2>/dev/null || true
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

            # Taskstream web app (`pnpm dev:web` -> Vite in apps/taskstream-web).
            ts-web = ''set -e; pnpm install; pnpm dev:web'';
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
          # path dependencies (`taskstream-desktop -> ../../libs/*`), so `cargo build -p
          # taskstream-desktop` fails in the sandbox with "failed to read
          # .../source/libs/acp-client/Cargo.toml". All outputs below build
          # from the full workspace source instead.
          crane = config.extraLib.craneLib;
          relPath = p: (/. + builtins.unsafeDiscardStringContext "${self.outPath + "${p}"}");
          # The version the binary is built with and the bundles are named
          # after: read from Cargo.toml, the single source of truth (see
          # scripts/version.sh for the non-nix readers of the same field).
          version = (builtins.fromTOML (builtins.readFile (relPath "/Cargo.toml"))).workspace.package.version;
          # `commonCargoSources` drops non-Rust files, but taskstream-desktop embeds its
          # icons via `include_bytes!` and storage embeds its migrations via
          # `include_dir!`, so keep those trees in the build source.
          fullSrc = lib.fileset.toSource {
            root = (/. + builtins.unsafeDiscardStringContext self.outPath);
            fileset = lib.fileset.unions [
              (crane.fileset.commonCargoSources (relPath "/"))
              (relPath "/apps/taskstream-desktop/assets")
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
          myDevShell.overrides.stdenv = pkgs.stdenvNoCC;
          myDevShell.buildInputs = buildDeps ++ devDeps ++ (attrValues scripts);
          myDevShell.shellHooks = { };
          myDevShell.cleanups.icons.script = ''rm -f target/debug/taskstream.app/Contents/Resources/taskstream.icns'';

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
          checks.taskstream-desktop-tests = crane.cargoTest {
            src = fullSrc;
            cargoArtifacts = fullDeps;
            nativeBuildInputs = config.rust.nativeBuildInputs ++ [
              pkgs.git
              # The coding-agent tests resolve the `opencode` binary from PATH
              # (the devshell carries it for the same reason).
              pkgs.opencode
              # Sandbox fixtures below: zoneinfo database and CA bundle.
              pkgs.tzdata
              pkgs.cacert
            ];
            buildInputs = config.rust.buildInputs;
            pname = "taskstream-desktop-tests";
            inherit version;
            cargoTestExtraArgs = "-p taskstream-desktop -p storage -p acp-client -p gpui_tokio";
            env = config.rust.buildEnv // {
              GIT_AUTHOR_NAME = "CI";
              GIT_AUTHOR_EMAIL = "ci@taskstream.invalid";
              GIT_COMMITTER_NAME = "CI";
              GIT_COMMITTER_EMAIL = "ci@taskstream.invalid";
              # Debug info for build scripts (e.g. rustls), so a failing
              # `cargoTest` phase reports usable backtraces.
              CARGO_PROFILE_TEST_BUILD_OVERRIDE_DEBUG = "true";
              # The sandbox has no /usr/share/zoneinfo or /etc/ssl/certs:
              # without these, jiff falls back to UTC (breaking the
              # America/New_York repeat test) and reqwest refuses to build a
              # client ("No CA certificates were loaded from the system").
              TZDIR = "${pkgs.tzdata}/share/zoneinfo";
              SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
            };
          };

          # Linux binary of taskstream-desktop built with one expression on any host:
          # native on a Linux host, cross-compiled on macOS. Rust cross needs
          # the target std plus a linker/C toolchain for the target; `zig cc`
          # provides both C/C++/ar behind wrapper scripts, so no per-host
          # pkgsCross stdenv is required. `doCheck` stays off: cross-built
          # test binaries cannot run on the build host; `checks.taskstream-desktop-tests`
          # runs the suites natively per runner instead.
          # A single `packages` set: Nix forbids mixing `packages.x` entries
          # with a wholesale `packages` assignment in one attrset.
          packages =
            {
              taskstream-desktop-linux =
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
                  # Building `-p taskstream-desktop` directly only needs its own closure.
                linuxCrane.buildPackage {
                  src = fullSrc;
                  inherit (config.rust) nativeBuildInputs;
                  buildInputs = config.rust.buildInputs;
                  pname = "taskstream-desktop-linux";
                  inherit version;
                  cargoExtraArgs = "-p taskstream-desktop";
                  doCheck = false;
                  # `zig cc` resolves its cache dir via HOME, which the build env
                  # leaves unset/unwritable; point it at the per-build temp dir.
                  preBuild = ''export HOME="$TMPDIR"'';
                  env = config.rust.buildEnv // crossEnv;
                };

              taskstream-desktop =
                lib.mkForce (crane.buildPackage {
                  src = fullSrc;
                  cargoArtifacts = fullDeps;
                  inherit (config.rust) nativeBuildInputs;
                  buildInputs = config.rust.buildInputs;
                  pname = "taskstream-desktop";
                  inherit version;
                  cargoExtraArgs = "-p taskstream-desktop";
                  doCheck = false;
                  env = config.rust.buildEnv;
                });
            }
            // lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
              # The macOS app bundle as a `nix build` output, so darwin is
              # packaged the way Linux already is (packages.taskstream-linux-dist)
              # and the bundle can be installed by home-manager, which links
              # `$out/Applications/*.app` into the profile and therefore into
              # the Dock.
              #
              # It is the same scripts/package-macos.sh the CI release job runs,
              # driven over the binary `taskstream-desktop` builds. Its icon step
              # falls back to rsvg-convert + png2icns (scripts/make-icns.sh),
              # because a sandbox never sees sips or iconutil; signing is skipped
              # for the same reason, and is not needed by a locally built bundle.
              taskstream-desktop-app =
                let
                  # Only what assembling the bundle reads: the plist and artwork
                  # it copies, and the scripts that render the icon and lay the
                  # .app out.
                  packagingSrc = lib.fileset.toSource {
                    root = (/. + builtins.unsafeDiscardStringContext self.outPath);
                    fileset = lib.fileset.unions [
                      (relPath "/apps/taskstream-desktop/assets/Info.plist")
                      (relPath "/apps/taskstream-desktop/assets/icons/do-list-app.svg")
                      (relPath "/scripts/package-macos.sh")
                      (relPath "/scripts/make-icns.sh")
                      (relPath "/scripts/icon-grain.py")
                      (relPath "/scripts/version.sh")
                    ];
                  };
                in
                pkgs.stdenvNoCC.mkDerivation {
                  pname = "taskstream-desktop-app";
                  inherit version;
                  src = packagingSrc;
                  nativeBuildInputs = [ pkgs.librsvg pkgs.libicns pkgs.python3 ];
                  # A bundle is copied out of the store as it is: stripping or
                  # rewriting the Mach-O inside it would break it.
                  dontFixup = true;
                  buildPhase = ''
                    runHook preBuild
                    bash scripts/package-macos.sh \
                      --binary ${config.packages.taskstream-desktop}/bin/taskstream-desktop \
                      --out "$PWD/dist" \
                      --version ${version} \
                      --no-archives
                    runHook postBuild
                  '';
                  installPhase = ''
                    runHook preInstall
                    mkdir -p $out/Applications
                    cp -R dist/taskstream.app $out/Applications/
                    runHook postInstall
                  '';
                  meta = {
                    description = "taskstream desktop, as a macOS app bundle";
                    platforms = lib.platforms.darwin;
                  };
                };
            }
            // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
              # Linux release artifacts (tarball, .deb, AppImage) as a `nix build`
              # output: runs scripts/package-linux.sh inside the sandbox with every
              # tool from nixpkgs, so CI needs no devshell, no `--impure`, and no
              # curl'd appimagetool (it arrives via `fetchurl`, pinned by hash).
              # Linux-only: appimagetool is an x86_64 executable and ldd/dpkg-deb
              # only make sense there.
              taskstream-linux-dist =
                let
                  # Pinned out-of-store tool, fetched purely by content hash
                  # (same revision the workflow used to curl: 1.9.1).
                  appimagetool = pkgs.fetchurl {
                    url = "https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage";
                    sha256 = "ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0";
                  };
                  # The runtime ELF appimagetool prepends to the AppImage, passed
                  # on as --runtime-file. Left to itself appimagetool downloads it
                  # from type2-runtime's releases, which a sandbox forbids; it is
                  # a tagged release rather than the moving `continuous` tag so
                  # the hash keeps meaning what it says. x86_64 matches the
                  # architecture this derivation packs today.
                  appimageRuntime = pkgs.fetchurl {
                    url = "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64";
                    sha256 = "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d";
                  };
                  packagingSrc = lib.fileset.toSource {
                    root = (/. + builtins.unsafeDiscardStringContext self.outPath);
                    fileset = lib.fileset.unions [
                      (relPath "/scripts/package-linux.sh")
                      (relPath "/apps/taskstream-desktop/assets")
                    ];
                  };
                in
                pkgs.stdenv.mkDerivation {
                  pname = "taskstream-linux-dist";
                  inherit version;
                  src = packagingSrc;
                  # Everything package-linux.sh shells out to: ldd (glibc),
                  # objdump (binutils), dpkg-deb, rsvg-convert; plus the tools
                  # appimagetool prefers to have around (desktop-file-validate,
                  # file for arch sniffing).
                  nativeBuildInputs = [
                    pkgs.glibc
                    pkgs.binutils
                    pkgs.dpkg
                    pkgs.librsvg
                    pkgs.desktop-file-utils
                    pkgs.file
                  ];
                  # $out holds archives and an AppImage (whose leading bytes are
                  # the runtime ELF): stripping would corrupt the AppImage, and
                  # there is nothing in there with symbols worth stripping.
                  dontStrip = true;
                  buildPhase = ''
                    appimage=$PWD/appimagetool
                    cp -f ${appimagetool} "$appimage"
                    chmod +x "$appimage"
                    PACKAGE_LINUX_SKIP_STORE_CHECK=1 bash scripts/package-linux.sh \
                      --binary ${config.packages.taskstream-desktop}/bin/taskstream-desktop \
                      --version ${version} \
                      --out dist \
                      --appimagetool "$appimage" \
                      --appimage-runtime ${appimageRuntime}
                  '';
                  installPhase = ''
                    mkdir -p $out
                    cp -f dist/* $out/
                  '';
                };
            };
        };
    });

}
