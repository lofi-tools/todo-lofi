{
  # inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # inputs.flake-utils.url = "github:numtide/flake-utils";
  # inputs.crane = { url = "github:ipetkov/crane"; };
  # inputs.rust-overlay = { url = "github:oxalica/rust-overlay"; inputs.nixpkgs.follows = "nixpkgs"; };
  inputs.nixpkgs.url = "github:cachix/devenv-nixpkgs/rolling";
  inputs.parts.url = "github:hercules-ci/flake-parts";
  inputs.my-nix = { url = "github:nmrshll/nix-utils"; inputs.nixpkgs.follows = "nixpkgs"; inputs.fp.follows = "parts"; };


  outputs = inputs@{ self, parts, my-nix, ... }: parts.lib.mkFlake { inherit inputs; } (top@{ lib, ... }:
    with builtins; {
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      imports = (attrValues inputs.my-nix.flakeModules) ++ [ ];
      perSystem = { pkgs, system, lib, l, self', ... }:
        let
          bin = inputs.my-nix.bin.${system} // (mapAttrs (n: p: "${p}/bin/${n}") scripts);

          buildDeps = [
            pkgs.pkg-config
          ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
            # pkgs.darwin.apple_sdk.frameworks.Security
            # pkgs.darwin.apple_sdk.frameworks.CoreServices
            # pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
            # pkgs.darwin.apple_sdk.frameworks.WebKit
            # pkgs.darwin.apple_sdk.frameworks.AppKit
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            # pkgs.webkitgtk
            # pkgs.gtk3
            # pkgs.cairo
            # pkgs.gdk-pixbuf
            # pkgs.glib
            # pkgs.dbus
            # pkgs.openssl_3
            # pkgs.librsvg
            # pkgs.libsoup_3
          ];
          devDeps = [ pkgs.cargo-tauri pkgs.cargo-watch ];

          wd = "$(git rev-parse --show-toplevel)";
          scripts = mapAttrs (n: s: pkgs.writeShellScriptBin n s) { };



          # # TODO move to my-nix
          # workspaceMembers = (fromTOML (readFile ./Cargo.toml)).workspace.members or [ ];
          # expandWsMember = member:
          #   if lib.strings.hasSuffix "/*" member then
          #     let
          #       baseDir = lib.strings.removeSuffix "/*" member;
          #       subDirs = lib.filterAttrs (name: type: type == "directory") (readDir (./. + "/${baseDir}"));
          #     in
          #     map (name: "${baseDir}/${name}") (attrNames subDirs)
          #   else
          #     [ member ];

          # validCratePaths = filter (crate: pathExists (./. + "/${crate}/Cargo.toml")) (concatLists (map expandWsMember workspaceMembers));
          # crates = l.listToAttrs (map (path: { name = baseNameOf path; value = l.customRust.buildCrate path; }) validCratePaths);



        in
        {
          # packages = scripts;
          # checks = tests;
          devShellParts.env = { };
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
