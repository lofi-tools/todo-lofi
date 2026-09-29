{ ... }: {
  perSystem = { pkgs, ... }: {
    # The coding-agent tests resolve the `opencode` binary from PATH (and the
    # app spawns it at run time), so the shell carries one; without it `nix
    # develop` can run every test but those.
    #
    # scripts/package-linux.sh also calls these, and two of them (rsvg-convert,
    # dpkg-deb) are not in the base shell: the icon step degrades without the
    # first, the .deb step fails without the second.
    myDevShell.buildInputs =
      [ pkgs.opencode ]
      ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
        pkgs.binutils
        pkgs.dpkg
        pkgs.librsvg
      ];

    devShells = pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
      # Cross-compiling to windows-gnu, for the packaging job in
      # .github/workflows/ci.yml. The mingw toolchain comes from pkgsCross and
      # the windows std is an extra target on the same rust toolchain, so cargo
      # needs the linker named per target. Only a Linux host can build it, which
      # is why the shell is defined only there.
      todo-2-windows =
        let
          mingw = pkgs.pkgsCross.mingwW64.stdenv.cc;
          toolchain = pkgs.rust-bin.stable.latest.default.override {
            targets = [ "x86_64-pc-windows-gnu" ];
          };
        in
        pkgs.mkShell {
          packages = [ mingw toolchain ];
          CARGO_BUILD_TARGET = "x86_64-pc-windows-gnu";
          CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "${mingw}/bin/x86_64-w64-mingw32-gcc";
          CC_x86_64_pc_windows_gnu = "${mingw}/bin/x86_64-w64-mingw32-gcc";
          CXX_x86_64_pc_windows_gnu = "${mingw}/bin/x86_64-w64-mingw32-g++";
          AR_x86_64_pc_windows_gnu = "${mingw}/bin/x86_64-w64-mingw32-ar";
          # Statically link the mingw runtime so the .exe is self-contained;
          # scripts/package-windows.sh still ships any DLL left beside it.
          RUSTFLAGS = "-C target-feature=+crt-static";
        };
    };
  };
}
