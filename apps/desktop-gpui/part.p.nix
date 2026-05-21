{ l, ... }: {
  perSystem = { pkgs, ... }: with builtins; let
    # bin = (mapAttrs (n: p: "${p}/bin/${n}") scripts);
    buildDeps = [
      # pkgs.pkg-config
      # inputs.exo.packages.${system}.metal-toolchain
      # pkgs.darwin.xcode_26_1_Apple_silicon # comment out for first build
      # pkgs.apple-sdk_26
      # TODO try symlinkJoin of xcode and exo.metal-toolchain
      # pkgs.own.my-nix.install-xcode-global
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
    runtimeDeps = [ ];
    devDeps = [ ];

    # wd = "$(git rev-parse --show-toplevel)";
    scripts = mapAttrs pkgs.writeShellScriptBin {
      pdg = "cargo run -p desktop-gpui";
      dbg-env = '' ${concatStringsSep "\n" (attrValues (mapAttrs (n: v: "printf \"${n}=${v}\\n\"") env))} '';
      dbg-store-xcode = '' DEVELOPER_DIR="${pkgs.own.my-nix.install-xcode-global.DEV_DIR}" xcodebuild -version '';
      # xcrun = ''${env.DEVELOPER_DIR}/Contents/Developer/usr/bin/xcrun'';
      # metal = ''${env.DEVELOPER_DIR}/Toolchains/XcodeDefault.xctoolchain/usr/bin/metal $@'';
    };

    env = {
      # DEVELOPER_DIR = pkgs.own.my-nix.install-xcode-global.DEV_DIR;
      # SDKROOT = pkgs.own.my-nix.install-xcode-global.SDKROOT;
      # NIX_LDFLAGS = "-F/System/Library/Frameworks -F/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks";
      # # Tells the compiler where to find the headers if a build script (-sys crate) compiles C/C++ code
      # BINDGEN_EXTRA_CLANG_ARGS = "-F/System/Library/Frameworks";

      # DEVELOPER_DIR = "${unsafeDiscardStringContext pkgs.darwin.xcode_26_1_Apple_silicon}/Contents/Developer";
      # SDKROOT = "${env.DEVELOPER_DIR}/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
      # SDKROOT = "${inputs.exo.packages.${system}.metal-toolchain}";
      # METAL = "${inputs.exo.packages.${system}.metal-toolchain}/bin/metal";
      # BINDGEN_EXTRA_CLANG_ARGS = "-I${inputs.exo.packages.${system}.metal-toolchain}";
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
      configure_C = l.concatStringsSep "\n" (l.mapAttrsToList (name: value: "export ${name}=\"${value}\"") env);
      # install-xcode = '' ${pkgs.own.my-nix.install-xcode-global}/bin/install-xcode-global '';
      # # install-metal = ''xcodebuild -importComponent metalToolchain -importPath .cache/Metal.dmg'';
      # # use-os-xcrun = ''export PATH="/usr/bin:/usr/local/bin:$PATH"'';
      # #     DEVELOPER_DIR = unsafeDiscardStringContext DEV_DIR;
      # # SDKROOT = unsafeDiscardStringContext "${DEV_DIR}/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk";
      # force-sdk-dirs = ''
      #   export DEVELOPER_DIR="${env.DEVELOPER_DIR}"
      #   export SDKROOT="${env.SDKROOT}"
      #   export MACOSX_DEPLOYMENT_TARGET="15.0"
      #   # Explicitly expose the global metal toolchain binary directly to the path
      #   export PATH="/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin:$PATH"
      #   export TOOLCHAINS=default
      # '';
    };
    # adding rust from rust-overlay auto-patches CC, makes xcodebuild/metal fail:
    # solution: devShells.default = pkgs.mkShell.override { stdenv = customStdenv; }
    myDevShell.overrides.stdenv = pkgs.stdenvNoCC;
  };
}
