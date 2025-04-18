{
  description = "A Rust project with todo-core";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    flake-parts.url = "github:hercules-ci/flake-parts";
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
  };

  outputs = inputs@{ self, nixpkgs, flake-utils, flake-parts, crane, rust-overlay, ... }:
    flake-parts.lib.mkFlake { inherit self; } {
      systems = flake-utils.lib.defaultSystems;
      imports = [ ]; # Add any flake-parts modules here if needed

      perSystem = { config, pkgs, system, ... }:
        let
          overlays = [ (import rust-overlay) ];
          pkgs' = import nixpkgs {
            inherit system overlays;
          };
          rustToolchain = pkgs'.rust-bin.stable.latest.default.override {
            extensions = [ "rust-src" ];
          };
          craneLib = crane.lib.${system}.overrideToolchain rustToolchain;

          # Build the workspace members
          src = pkgs.lib.cleanSource ./.;
          commonArgs = {
            inherit src;
            nativeBuildInputs = with pkgs; [ pkg-config ]; # Add native build dependencies here
          };

          # Build the cargo artifacts
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          # Build the todo-core crate
          todo-core = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
            pname = "todo-core";
          });

        in
        {
          # Development shell
          devShells.default = pkgs'.mkShell {
            inputsFrom = [ cargoArtifacts ];
            nativeBuildInputs = with pkgs'; [
              rustToolchain
              cargo-watch # Optional: for development workflow
              pkg-config
              # Add other dev tools here
            ];
            # Add environment variables if needed
            # RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          };

          # Packages (optional, if you want to package binaries)
          # packages.default = todo-core; # Or specific binary crate

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
            # Add checks for other crates if needed
          };
        };
    };
}