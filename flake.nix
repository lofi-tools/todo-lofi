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

          bash.wd = "$(git rev-parse --show-toplevel)";
          scripts = mapAttrs (n: s: pkgs.writeShellScriptBin n s) {
            # prun = ''set -x; package="$1"; shift; cargo run -p "$package" -- $@'';
            dt = ''set -e;  cd desktop; cargo tauri dev '';
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
          };

          env = {
            # SNAFU_RAW_ERROR_MESSAGES = 1;
            # RUST_LIB_BACKTRACE = 1;
            # RUST_BACKTRACE = "1";
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
        };
    });

}
