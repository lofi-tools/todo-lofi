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
      perSystem = { pkgs, ... }:
        let
          # bin = inputs.my-nix.bin.${system} // (mapAttrs (n: p: "${p}/bin/${n}") scripts);
          buildDeps = [
            pkgs.pkg-config
          ];

          devDeps = [ pkgs.cargo-tauri pkgs.cargo-watch ];

          lsRegister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

          infoPlist = pkgs.writeText "Info.plist" ''
            <?xml version="1.0" encoding="UTF-8"?>
            <!DOCTYPE plist PUBLIC "-//Apple Computer//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            <plist version="1.0">
            <dict>
              <key>CFBundleDevelopmentRegion</key><string>English</string>
              <key>CFBundleDisplayName</key><string>todo-lofi</string>
              <key>CFBundleExecutable</key><string>todo-2</string>
              <key>CFBundleIconFile</key><string>todo-lofi.icns</string>
              <key>CFBundleIdentifier</key><string>io.github.lofi-tools.todo-lofi</string>
              <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
              <key>CFBundleName</key><string>todo-lofi</string>
              <key>CFBundlePackageType</key><string>APPL</string>
              <key>CFBundleShortVersionString</key><string>0.1.0</string>
              <key>CSResourcesFileMapped</key><true/>
              <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
              <key>LSMinimumSystemVersion</key><string>12.0</string>
              <key>LSRequiresCarbon</key><true/>
              <key>NSHighResolutionCapable</key><true/>
            </dict>
            </plist>
          '';

          todoAppWrapper = pkgs.runCommand "todo-lofi-app-wrapper" { } ''
            mkdir -p $out/Contents/Resources
            cp ${infoPlist} $out/Contents/Info.plist
            cp ${./apps/todo-2/assets/icons/do-list-app.svg} $out/Contents/Resources/todo-lofi.svg
          '';

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

            t2 = with bash; ''
              set -e
              APP_DIR="target/debug/todo-lofi.app"
              ICON="$APP_DIR/Contents/Resources/todo-lofi.icns"
              ICON_SVG="${wd}/apps/todo-2/assets/icons/do-list-app.svg"

              mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
              cp -f "${todoAppWrapper}/Contents/Info.plist" "$APP_DIR/Contents/Info.plist"

              # Regenerate whenever the artwork or this recipe changed, so an icon
              # baked earlier (qlmanage composites the SVG on a white matte) can't stick.
              ICON_SUM=$(cksum "$ICON" 2>/dev/null | cut -d' ' -f1,2)
              if [ ! -f "$ICON" ] || [ "$ICON_SVG" -nt "$ICON" ] || [ "${wd}/flake.nix" -nt "$ICON" ]; then
                # sips rasterizes an SVG at its intrinsic size, so paint the icon
                # on a 1024px canvas instead of upscaling a 155px bitmap; sips
                # keeps the canvas transparent where the SVG has no fill.
                ICON_TMP=$(mktemp -d)
                ICONSET="$ICON_TMP/icon.iconset"
                mkdir -p "$ICONSET"
                sed 's|<svg |<svg width="1024" height="1024" |' "$ICON_SVG" > "$ICON_TMP/icon-1024.svg"
                sips -s format png "$ICON_TMP/icon-1024.svg" --out "$ICONSET/master.png" >/dev/null
                # Grain is composited into the raster rather than committed in the SVG,
                # so the artwork stays clean for other consumers and only the built
                # icon pays for the texture (cell px at 1024, amplitude, seed).
                python3 "${wd}/scripts/icon-grain.py" "$ICONSET/master.png" 20 5 11
                for size in 16 32 128 256 512; do
                  sips -z "$size" "$size" "$ICONSET/master.png" --out "$ICONSET/icon_''${size}x''${size}.png" >/dev/null
                  sips -z "$(( size * 2 ))" "$(( size * 2 ))" "$ICONSET/master.png" --out "$ICONSET/icon_''${size}x''${size}@2x.png" >/dev/null
                done
                # The master already is the 1024px rep; copying it keeps the grained
                # pixels instead of letting sips re-encode noise (which grows it ~20%).
                cp "$ICONSET/master.png" "$ICONSET/icon_512x512@2x.png"
                rm -f "$ICONSET/master.png"
                iconutil -c icns "$ICONSET" -o "$ICON"
                rm -rf "$ICON_TMP"

                # IconServices caches rendered tiles, so the Dock goes on painting
                # the previous icon until the bundle is re-registered and the Dock
                # restarts. Only pay for that when the icon actually changed.
                if [ "$ICON_SUM" != "$(cksum "$ICON" | cut -d' ' -f1,2)" ]; then
                  touch "$APP_DIR" "$ICON"
                  "${lsRegister}" -f "$APP_DIR" 2>/dev/null || true
                  killall Dock 2>/dev/null || true
                fi
              fi

              # open hands the bundle to launchd, so the app is neither a child of
              # this script nor in its process group: signals sent here never reach
              # it. Kill it by process name instead - launchd resolves the
              # Contents/MacOS symlink, so "-f Contents/MacOS/todo-2" would only
              # ever match cargo-watch's own command line (killing the watcher).
              # The app is also launched directly (not via `open`, which swallows
              # stdout/stderr into launchd) so tracing logs stream to this terminal.
              watcher=""
              app_pid=""
              stop_app() {
                [ -z "$watcher" ] || kill "$watcher" 2>/dev/null || true
                [ -z "$app_pid" ] || kill "$app_pid" 2>/dev/null || true
                pkill -x todo-2 2>/dev/null || true
              }
              trap stop_app EXIT INT TERM HUP

              # The devshell exports CARGO_BUILD_TARGET, so cargo writes to
              # target/<triple>/debug; without it, to target/debug. The bundle
              # symlink must point at the artifact cargo actually produces,
              # otherwise `open` keeps launching a stale binary.
              BIN_DIR="target/debug"
              if [ -n "''${CARGO_BUILD_TARGET:-}" ]; then
                BIN_DIR="target/''${CARGO_BUILD_TARGET}/debug"
              fi

              # Build, link binary, launch
              cargo build -p todo-2
              ln -sf "$(pwd)/$BIN_DIR/todo-2" "$APP_DIR/Contents/MacOS/todo-2"
              # Sign with the self-signed todo-lofi-dev identity so Gatekeeper
              # doesn't throttle every launch; strip any quarantine flag.
              if security find-identity -v -p codesigning 2>/dev/null | grep -q "todo-lofi-dev"; then
                codesign --force --sign "todo-lofi-dev" "$BIN_DIR/todo-2" 2>/dev/null || true
                codesign --force --deep --sign "todo-lofi-dev" "$APP_DIR" 2>/dev/null || true
              fi
              xattr -dr com.apple.quarantine "$APP_DIR" "$BIN_DIR/todo-2" 2>/dev/null || true
              # Direct launch keeps the app's stdout/stderr on this terminal;
              # `open` would detach it under launchd and hide the logs.
              "$APP_DIR/Contents/MacOS/todo-2" &
              app_pid=$!

              # Run the watcher in the background and wait: bash defers traps while
              # a foreground command runs, so killing the script would leave both
              # the watcher and the app alive. wait is interrupted by the signal,
              # letting the trap take them down.
              cargo watch -x "build -p todo-2" -s 'codesign --force --sign "todo-lofi-dev" "target/debug/todo-2" 2>/dev/null || true; xattr -dr com.apple.quarantine "target/debug/todo-lofi.app" 2>/dev/null || true; pkill -x todo-2 2>/dev/null || true; sleep 0.3; "target/debug/todo-lofi.app/Contents/MacOS/todo-2" &' &
              watcher=$!
              wait "$watcher"
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
        {
          # packages = scripts;
          rust.buildInputs = buildDeps;
          rust.buildEnv = env;
          # rust.toolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
          #   extensions = [ "rust-src" "rust-analyzer" ];
          #   targets = [ ];
          # });
          myDevShell.env = env;
          myDevShell.buildInputs = buildDeps ++ devDeps ++ (attrValues scripts);
          myDevShell.shellHooks = { };
          myDevShell.cleanups.icons.script = ''rm -f target/debug/todo-lofi.app/Contents/Resources/todo-lofi.icns'';
        };
    });

}
