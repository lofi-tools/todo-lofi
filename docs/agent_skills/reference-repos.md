---
name: reference-repos
description: Inspect Taskstream's named reference repositories (pandacode, herdr, herdr-core, herdr-app, and pi) when using them as implementation examples or comparing designs.
---

Resolve reference names using `referenceRepos` in the repository root's `flake.nix`.
The list is the source of truth for names and URLs; `checkout-references --list`
prints the same mapping without downloading anything.

From the Taskstream repository, run `checkout-references <name>` in its Nix dev
shell to download a missing reference, or `checkout-references` for all references.
Outside the shell, use `nix develop -c checkout-references <name>`. Checkouts live
at `<taskstream-root>/.cache/checkout/<name>` regardless of the caller's directory
within Taskstream. Existing checkouts are preserved, so they may be older than the
remote default branch.

Search only the requested checkout with `rg --files` and `rg`, then read relevant
files. Treat the checkout as reference material: keep implementation changes in
Taskstream unless the user requests otherwise. Cite the reference name, relative
file path, and checkout commit (`git -C .cache/checkout/<name> rev-parse HEAD`)
when explaining findings. Do not assume separate Herdr repositories share the
same architecture or API; inspect the named repository.

If a checkout is unavailable or downloading fails, report it and use available
references without inventing their contents. For an unknown name, consult the
configured list and ask for its URL before adding a new reference.
