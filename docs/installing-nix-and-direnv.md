# Installing Nix and direnv

This repository uses [Nix](https://nixos.org) (flakes) for its development
environment: the `.envrc` file loads `flake.nix`, which provides the Rust
toolchain, cargo-watch, and the helper scripts.  direnv with
[nix-direnv](https://github.com/nix-community/nix-direnv) makes this automatic
when you `cd` into the repo and caches the shell between visits.

## 1. Install Nix

Recommended — the Determinate Systems installer (flakes enabled by default):

```bash
curl -L https://install.determinate.systems/nix | sh -s -- install
```

or the official multi-user installer (then enable flakes in
`/etc/nix/nix.conf` with `experimental-features = nix-command flakes`):

```bash
curl -L https://nixos.org/nix/install | sh -s -- --daemon
```

Open a new shell afterwards so `nix` is on your `PATH`.

## 2. Install direnv

```bash
nix profile install nixpkgs#direnv   # or: brew install direnv / apt install direnv
```

Hook it into your shell (bash example; see `direnv hook --help` for others):

```bash
echo 'eval "$(direnv hook bash)"' >> ~/.bashrc
```

## 3. Enable nix-direnv (recommended)

nix-direnv caches the built devShell, so re-entering the repo is fast.  Manual
install:

```bash
git clone https://github.com/nix-community/nix-direnv ~/.config/direnv/nix-direnv
echo 'source ~/.config/direnv/nix-direnv/direnvrc' >> ~/.config/direnv/direnvrc
```

(or install the `nix-direnv` Home Manager / NixOS module instead).

## 4. Allow the environment

From the repository root:

```bash
direnv allow
```

The first `direnv allow` builds the devShell and takes a while; later shells
load from the nix-direnv cache.  Verify it worked:

```bash
direnv status        # should show "Found RC allowed true"
which cargo rustc    # should resolve into the Nix store
```

## Troubleshooting

- `direnv reload` re-evaluates `.envrc` after a change (e.g. `flake.lock`).
- `direnv status` shows why the environment is (not) loaded.
- The `.envrc` uses `use flake . --impure` and, if `~/src/scripts/my-nix`
  exists, overrides the `my-nix` flake input with that local checkout.
- If builds fail on a fresh lockfile, try `direnv reload` or
  `nix flake update` in the repo.