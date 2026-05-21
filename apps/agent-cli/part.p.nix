{ l, ... }: {
  perSystem = { pkgs, ... }: with builtins; let
    # bin = (mapAttrs (n: p: "${p}/bin/${n}") scripts);
    buildDeps = [
      pkgs.llvm
      # Use unwrapped clang to avoid cc-wrapper conflicts
      pkgs.llvmPackages_22.clang-unwrapped
    ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
    ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
    ];
    runtimeDeps = [
    ];
    devDeps = [
    ];

    # wd = "$(git rev-parse --show-toplevel)";
    scripts = mapAttrs pkgs.writeShellScriptBin {
      ag = "cargo run -p agent-cli";
    };

    env = {
      # bypass Nix's cc-wrapper entirely
      CC = "${pkgs.llvmPackages_22.clang-unwrapped}/bin/clang";
      CXX = "${pkgs.llvmPackages_22.clang-unwrapped}/bin/clang++";
      # Prevent cc-rs from adding --target that triggers wrapper warnings
      CARGO_BUILD_TARGET = "aarch64-apple-darwin";
      MACOSX_DEPLOYMENT_TARGET = "14.0";
    };
  in
  {
    packages = scripts;
    # pkgs.overlays = [ (import config.flakeInputsOf.my-nix.rust-overlay) ];
    rust.buildInputs = runtimeDeps;
    rust.nativeBuildInputs = buildDeps;
    rust.buildEnv = env;
    myDevShell.env = env;
    myDevShell.buildInputs = buildDeps ++ runtimeDeps ++ devDeps ++ (attrValues scripts);
    myDevShell.shellHooks = {
      agent-cli-force-env = concatStringsSep "\n" (l.mapAttrsToList (n: v: "${n}=${v}") env);
    };
  };
}
