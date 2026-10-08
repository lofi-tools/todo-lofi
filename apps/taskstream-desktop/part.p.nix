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
      ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [
        pkgs.binutils
        pkgs.dpkg
        pkgs.librsvg
      ];
  };
}
