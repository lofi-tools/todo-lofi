{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    flake-parts.url = "github:hercules-ci/flake-parts";
    crane = { url = "github:ipetkov/crane"; inputs.nixpkgs.follows = "nixpkgs"; };
    rust-overlay = { url = "github:oxalica/rust-overlay"; inputs.nixpkgs.follows = "nixpkgs"; inputs.flake-utils.follows = "flake-utils"; };
  };

  outputs = inputs@{ self, nixpkgs, flake-utils, flake-parts, crane, rust-overlay, ... }:
    with builtins; flake-parts.lib.mkFlake { inherit inputs; } {
      systems = flake-utils.lib.defaultSystems;
      imports = [ ]; # Add any flake-parts modules here if needed

      perSystem = { config, pkgs, system, lib, ... }:
        let
          ownPkgs = {
            rust = pkgs.rust-bin.stable.latest.default.override {
              extensions = [ "rust-src" ];
            };
          };

          craneLib = (crane.mkLib pkgs).overrideToolchain ownPkgs.rust;
          commonArgs = {
            src = pkgs.lib.cleanSource ./.;
            pname = "todo-lofi";
            nativeBuildInputs = buildDeps; # Add native build dependencies here
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          todo-core = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            pname = "todo-core";
          });

          buildDeps = [ ownPkgs.rust pkgs.pkg-config ];
          devDeps = [ pkgs.cargo-watch ];

          scripts = mapAttrs (name: txt: pkgs.writeShellScriptBin name txt) {
            prun = ''set -x; package="$1"; shift; cargo run -p "$package" -- $@'';
          };

        in
        {
          devShells.default = pkgs.mkShell {
            inputsFrom = [ cargoArtifacts ];
            buildInputs = buildDeps ++ devDeps ++ attrValues scripts;
          };

          # Checks: build and test
          checks = {
            # Check that todo-core builds
            inherit todo-core;
            # Check that todo-core tests pass
            todo-core-tests = craneLib.cargoNextest (commonArgs // {
              inherit cargoArtifacts;
              packageFilter = [ "todo-core" ]; # Specify the package to test
              doCheck = true;
            });
          };

          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ (import rust-overlay) ];
            config = { };
          };
        };
    };
}
