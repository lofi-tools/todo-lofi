{
  # inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # inputs.flake-utils.url = "github:numtide/flake-utils";
  # inputs.crane = { url = "github:ipetkov/crane"; };
  # inputs.rust-overlay = { url = "github:oxalica/rust-overlay"; inputs.nixpkgs.follows = "nixpkgs"; };
  inputs.nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
  inputs.parts.url = "github:hercules-ci/flake-parts";
  inputs.my-nix = { url = "github:nmrshll/nix-utils"; inputs.nixpkgs.follows = "nixpkgs"; inputs.fp.follows = "parts"; };

  # inputs.exo.url = "github:exo-explore/exo";
  # nixConfig = {
  #   extra-trusted-public-keys = "exo.cachix.org-1:okq7hl624TBeAR3kV+g39dUFSiaZgLRkLsFBCuJ2NZI=";
  #   extra-substituters = "https://exo.cachix.org";
  # };


  outputs = inputs@{ self, parts, my-nix, ... }: parts.lib.mkFlake { inherit inputs; } (top@{ lib, ... }:
    with builtins; {
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      imports = (attrValues inputs.my-nix.flakeModules) ++ [ ];
      perSystem = { pkgs, system, lib, l, self', ownPkgs, ... }:
        let
          bin = inputs.my-nix.bin.${system} // (mapAttrs (n: p: "${p}/bin/${n}") scripts);

          buildDeps = [
            pkgs.pkg-config
          ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
            # inputs.exo.packages.${system}.metal-toolchain
            # pkgs.darwin.xcode_26_1_Apple_silicon # comment out for first build
            # pkgs.apple-sdk_26
            # TODO try symlinkJoin of xcode and exo.metal-toolchain
            ownPkgs.install-xcode-global
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            /*  pkgs.webkitgtk */
            /*  pkgs.gtk3 */
            /*  pkgs.cairo */
            /*  pkgs.gdk-pixbuf */
            /*  pkgs.glib */
            /*  pkgs.dbus */
            /*  pkgs.openssl_3 */
            /*  pkgs.librsvg */
            /*  pkgs.libsoup_3 */
          ];

          devDeps = [ pkgs.cargo-tauri pkgs.cargo-watch ];

          wd = "$(git rev-parse --show-toplevel)";
          scripts = mapAttrs (n: s: pkgs.writeShellScriptBin n s) {
            xcrun = ''${env.DEVELOPER_DIR}/Contents/Developer/usr/bin/xcrun'';
            # metal = ''${env.DEVELOPER_DIR}/Toolchains/XcodeDefault.xctoolchain/usr/bin/metal $@'';
            dbg-env = '' ${concatStringsSep "\n" (attrValues (mapAttrs (n: v: "printf \"${n}=${v}\\n\"") env))} '';
            dbg-store-xcode = '' DEVELOPER_DIR="${ownPkgs.install-xcode-global.DEV_DIR}" xcodebuild -version '';
          };

          env = {
            # DEVELOPER_DIR = "${unsafeDiscardStringContext pkgs.darwin.xcode_26_1_Apple_silicon}/Contents/Developer";
            # SDKROOT = "${env.DEVELOPER_DIR}/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
            # SDKROOT = "${inputs.exo.packages.${system}.metal-toolchain}";
            # METAL = "${inputs.exo.packages.${system}.metal-toolchain}/bin/metal";
            # BINDGEN_EXTRA_CLANG_ARGS = "-I${inputs.exo.packages.${system}.metal-toolchain}";

            DEVELOPER_DIR = ownPkgs.install-xcode-global.DEV_DIR;
            SDKROOT = ownPkgs.install-xcode-global.SDKROOT;
          };

        in
        {
          # packages = scripts;
          rust.buildInputs = buildDeps;
          rust.buildEnv = env;

          devShellParts.env = env;
          devShellParts.shellHookParts = {
            install-xcode = '' ${ownPkgs.install-xcode-global}/bin/install-xcode-global '';
            # install-metal = ''xcodebuild -importComponent metalToolchain -importPath .cache/Metal.dmg'';
            # use-os-xcrun = ''export PATH="/usr/bin:/usr/local/bin:$PATH"'';
            #     DEVELOPER_DIR = unsafeDiscardStringContext DEV_DIR;
            # SDKROOT = unsafeDiscardStringContext "${DEV_DIR}/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
            appleSdkDirs = '' 
              export DEVELOPER_DIR="${env.DEVELOPER_DIR}"
              export SDKROOT="${env.SDKROOT}"
            '';
          };
          devShellParts.buildInputs = buildDeps ++ devDeps ++ (attrValues scripts);
        };
    });

}

#   outputs = inputs@{ self, nixpkgs, flake-utils, flake-parts, crane, rust-overlay, ... }:
#     with builtins; flake-parts.lib.mkFlake { inherit inputs; } {
#       systems = flake-utils.lib.defaultSystems;
#       imports = [ ]; # Add any flake-parts modules here if needed

#       perSystem = { config, pkgs, system, lib, ... }:
#         let
#           ownPkgs = {
#             rust = pkgs.rust-bin.stable.latest.default.override {
#               extensions = [ "rust-src" ];
#             };
#           };

#           craneLib = (crane.mkLib pkgs).overrideToolchain ownPkgs.rust;
#           commonArgs = {
#             src = pkgs.lib.cleanSource ./.;
#             pname = "todo-lofi";
#             nativeBuildInputs = buildDeps; # Add native build dependencies here
#           };
#           cargoArtifacts = craneLib.buildDepsOnly commonArgs;

#           todo-core = craneLib.buildPackage (commonArgs // {
#             inherit cargoArtifacts;
#             pname = "todo-core";
#           });

#           buildDeps = [
#             ownPkgs.rust
#             pkgs.pkg-config
#           ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
#             pkgs.darwin.apple_sdk.frameworks.Security
#             pkgs.darwin.apple_sdk.frameworks.CoreServices
#             pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
#             pkgs.darwin.apple_sdk.frameworks.WebKit
#             pkgs.darwin.apple_sdk.frameworks.AppKit
#           ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
#             pkgs.webkitgtk
#             pkgs.gtk3
#             pkgs.cairo
#             pkgs.gdk-pixbuf
#             pkgs.glib
#             pkgs.dbus
#             pkgs.openssl_3
#             pkgs.librsvg
#             pkgs.libsoup_3
#           ];
#           devDeps = [ pkgs.cargo-tauri pkgs.cargo-watch ];

#           scripts = mapAttrs (name: txt: pkgs.writeShellScriptBin name txt) {
#             prun = ''set -x; package="$1"; shift; cargo run -p "$package" -- $@'';
#             dt = ''set -e
#               cd desktop; cargo tauri dev
#             '';
#           };

#         in
#         {
#           devShells.default = pkgs.mkShell {
#             inputsFrom = [ cargoArtifacts ];
#             buildInputs = buildDeps ++ devDeps ++ attrValues scripts;
#           };

#           # Checks: build and test
#           checks = {
#             # Check that todo-core builds
#             inherit todo-core;
#             # Check that todo-core tests pass
#             todo-core-tests = craneLib.cargoNextest (commonArgs // {
#               inherit cargoArtifacts;
#               packageFilter = [ "todo-core" ]; # Specify the package to test
#               doCheck = true;
#             });
#           };

#           _module.args.pkgs = import inputs.nixpkgs {
#             inherit system;
#             overlays = [ (import rust-overlay) ];
#             config = { };
#           };
#         };
#     };
# }
