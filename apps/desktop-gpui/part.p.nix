{ l, config, ... }: {
  perSystem = { pkgs, ... }: with builtins; let
    # adding rust from rust-overlay auto-patches CC, makes xcodebuild/metal fail:
    # solution: devShells.default = pkgs.mkShell.override { stdenv = customStdenv; }
    customRust = pkgs.rust-bin.stable.latest.default.override {
      extensions = [ "rust-src" "rust-analyzer" ];
      targets = [ ];
    };
    craneLib = config.flakeInputsOf.my-nix.crane.mkLib pkgs;


    # bin = (mapAttrs (n: p: "${p}/bin/${n}") scripts);
    buildTimeDeps = [
      # pkgs.pkg-config
      customRust
      pkgs.cowsay
    ];
    runtimeDeps = [
      # pkgs.openssl
    ];
    devDeps = [
    ];

    # wd = "$(git rev-parse --show-toplevel)";
    scripts = mapAttrs pkgs.writeShellScriptBin {
      pdg = "cargo run -p desktop-gpui";
      dbg-env = '' ${concatStringsSep "\n" (attrValues (mapAttrs (n: v: "printf \"${n}=${v}\\n\"") env))} '';
      dbg-store-xcode = '' DEVELOPER_DIR="${pkgs.own.my-nix.install-xcode-global.DEV_DIR}" xcodebuild -version '';
    };

    env = {
      # DEVELOPER_DIR = pkgs.own.my-nix.install-xcode-global.DEV_DIR;
      # SDKROOT = pkgs.own.my-nix.install-xcode-global.SDKROOT;
      # NIX_LDFLAGS = "-F/System/Library/Frameworks -F/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks";
      # # Tells the compiler where to find the headers if a build script (-sys crate) compiles C/C++ code
      # BINDGEN_EXTRA_CLANG_ARGS = "-F/System/Library/Frameworks";
    };
  in
  {
    packages = scripts;
    pkgs.overlays = [ (import config.flakeInputsOf.my-nix.rust-overlay) ];
    # rust.buildInputs = runtimeDeps;
    # rust.nativeBuildInputs = buildTimeDeps;
    # rust.buildEnv = env;
    devShellParts.env = env;
    devShellParts.buildInputs = buildTimeDeps ++ runtimeDeps ++ devDeps ++ (attrValues scripts);
    devShellParts.shellHookParts = {
      configure_C = l.concatStringsSep "\n" (l.mapAttrsToList (name: value: "export ${name}=\"${value}\"") env);
    };
  };
}
