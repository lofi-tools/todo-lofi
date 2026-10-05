//! Git helpers for coding runs. The app owns the feature branch: it is cut
//! once the spec is approved and merged when the merge phase completes.
//!
//! Everything shells out to `git` with an explicit `current_dir` and captures
//! stderr so the details panel can show the user why a phase is blocked (no
//! `git2` dependency, matching the rest of the project). Every function is
//! synchronous and must be called from a blocking context.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The path a repo maps to on github.com, and the remote it was read from.
/// Resolved through [`resolve_remote`] rather than assuming `origin`: this
/// repo itself has `github` + `gitlab` remotes and no `origin`, so the sync
/// binding and the PR step both need to know which remote is the GitHub one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRef {
    pub name: String,
    pub owner: String,
    pub repo: String,
    pub url: String,
}

/// The entry `ensure_excluded` appends, exactly as git spells it.
const WORKTREE_IGNORE_ENTRY: &str = "worktrees/";

/// Run `git <args>` in `dir`, keeping the exit status separate from the output.
/// Only the callers that have to read a *failure* as a result need this —
/// `merge-tree` reports conflicts through a non-zero exit — so everything else
/// goes through [`run_git`] and never sees a status at all.
fn run_git_status(dir: &Path, args: &[&str]) -> anyhow::Result<(bool, String, String)> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run git: {e}"))?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

/// Run `git <args>` in `dir`, returning trimmed stdout. Failures carry the
/// captured stderr (falling back to stdout when git wrote nothing to stderr)
/// so the message the user sees is the one git produced.
fn run_git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let (succeeded, stdout, stderr) = run_git_status(dir, args)?;
    if !succeeded {
        let stderr = stderr.trim();
        let stdout = stdout.trim();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        anyhow::bail!("git {} failed: {detail}", args.join(" "));
    }
    Ok(stdout.trim().to_string())
}

/// Whether `dir` is inside a git working tree.
pub fn is_repo(dir: &Path) -> bool {
    run_git(dir, &["rev-parse", "--is-inside-work-tree"])
        .map(|out| out == "true")
        .unwrap_or(false)
}

/// The branch currently checked out ("HEAD" when detached).
pub fn current_branch(dir: &Path) -> anyhow::Result<String> {
    run_git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Changed paths (tracked modifications and untracked files). An empty vector
/// means the working tree is clean enough to switch and merge branches.
pub fn changed_paths(dir: &Path) -> anyhow::Result<Vec<String>> {
    let status = run_git(dir, &["status", "--porcelain"])?;
    Ok(status
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Whether a local branch of that name already exists.
pub fn branch_exists(dir: &Path, branch: &str) -> bool {
    run_git(dir, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .is_ok()
}

/// Check out an existing branch.
pub fn switch_branch(dir: &Path, branch: &str) -> anyhow::Result<()> {
    run_git(dir, &["switch", branch]).map(|_| ())
}

/// Merge `branch` into the currently checked-out branch with a merge commit,
/// so the feature's boundary stays visible in the history.
pub fn merge_no_ff(dir: &Path, branch: &str) -> anyhow::Result<()> {
    run_git(dir, &["merge", "--no-ff", branch]).map(|_| ())
}

/// Abort an in-progress merge, restoring the pre-merge tree.
pub fn abort_merge(dir: &Path) -> anyhow::Result<()> {
    run_git(dir, &["merge", "--abort"]).map(|_| ())
}

/// Delete a local branch, ignoring whether it was merged.
pub fn delete_branch(dir: &Path, branch: &str) -> anyhow::Result<()> {
    run_git(dir, &["branch", "-D", branch]).map(|_| ())
}

/// A remote name → URL, in `git remote` order.
fn remotes(dir: &Path) -> Vec<(String, String)> {
    let Ok(names) = run_git(dir, &["remote"]) else {
        return Vec::new();
    };
    names
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| {
            let url = run_git(dir, &["remote", "get-url", name]).ok()?;
            Some((name.to_string(), url))
        })
        .collect()
}

fn config_value(dir: &Path, key: &str) -> Option<String> {
    run_git(dir, &["config", "--get", key])
        .ok()
        .filter(|value| !value.is_empty())
}

/// The repo config key recording which run owns a branch. Branch ownership is
/// written into the repo rather than only into the database because the
/// question "is this branch mine to rewrite?" is asked *of a checkout* — the
/// same repo on another machine, or a copy of it, still has to answer, and the
/// rebase pass must never touch a branch this app did not create. The trade,
/// stated: the key is the branch name, so renaming a branch outside the app
/// orphans the claim, and an orphaned claim reads as "not ours", which fails
/// safe (nothing is rebased) rather than dangerous.
fn managed_branch_key(branch: &str) -> String {
    format!("branch.{branch}.taskstream-run")
}

/// Claim `branch` for `run_id`, so a later rebase pass knows the branch is the
/// app's to rewrite.
pub fn mark_branch_managed(repo_dir: &Path, branch: &str, run_id: u64) -> anyhow::Result<()> {
    run_git(
        repo_dir,
        &["config", &managed_branch_key(branch), &run_id.to_string()],
    )
    .map(|_| ())
}

/// The run that claims `branch`, or `None` when the repo does not record one.
pub fn managed_run_id(repo_dir: &Path, branch: &str) -> Option<u64> {
    config_value(repo_dir, &managed_branch_key(branch))?
        .parse()
        .ok()
}

/// Drop the claim once the branch is deleted, so a later branch created under
/// the same name is not mistaken for the deleted run's.
pub fn unmark_branch_managed(repo_dir: &Path, branch: &str) -> anyhow::Result<()> {
    let key = managed_branch_key(branch);
    // An absent key is the state this call wants; only `git config --unset`
    // would fail over it.
    if config_value(repo_dir, &key).is_none() {
        return Ok(());
    }
    run_git(repo_dir, &["config", "--unset", &key]).map(|_| ())
}

/// Rebase the branch checked out in `dir` onto `base`. `base` is the local ref
/// the branch was cut from, so this never fetches: pulling remote work into the
/// rebase would rewrite the branch onto commits the user has not seen, and
/// would turn "bring my branch up to date" into "someone else's work appeared in
/// my diff".
pub fn rebase_onto(dir: &Path, base: &str) -> anyhow::Result<()> {
    run_git(dir, &["rebase", base]).map(|_| ())
}

/// Abort an in-progress rebase. Every failing rebase is followed by this: a
/// branch left mid-rebase is detached and half-applied, which changes how every
/// later git command behaves (a `git status`, a push, and the agent pane's own
/// commands all read that state), so leaving one behind would corrupt the rest
/// of the run rather than just failing the step.
pub fn rebase_abort(dir: &Path) -> anyhow::Result<()> {
    run_git(dir, &["rebase", "--abort"]).map(|_| ())
}

/// The paths that would conflict if `from` were merged into `into`, computed
/// without touching either the worktree or the index. `git merge-tree
/// --write-tree` answers through its exit status — a non-zero one *is* the
/// conflict report — so this is the one caller that reads a failure as a
/// result. An empty vector means the merge would be clean.
pub fn merge_tree_conflicts(dir: &Path, into: &str, from: &str) -> anyhow::Result<Vec<String>> {
    let (succeeded, stdout, stderr) = run_git_status(
        dir,
        &["merge-tree", "--write-tree", "--no-messages", into, from],
    )?;
    if succeeded {
        return Ok(Vec::new());
    }
    let mut paths: Vec<String> = Vec::new();
    // Line 0 is the merged tree's object id; each conflict line is
    // `<mode> <object> <stage>\t<path>`.
    for line in stdout.lines().skip(1) {
        let Some((_, path)) = line.split_once('\t') else {
            continue;
        };
        let path = path.trim();
        if !path.is_empty() && !paths.iter().any(|seen| seen == path) {
            paths.push(path.to_string());
        }
    }
    if paths.is_empty() {
        // No conflict listing means the command itself did not work (an
        // unknown ref, unrelated histories, a git too old for `--write-tree`).
        // Reporting that as "no conflicts" would be a silent lie.
        anyhow::bail!("git merge-tree failed: {}", stderr.trim());
    }
    Ok(paths)
}

/// Parse `owner/repo` out of a github.com remote URL. Handles the scp-like
/// SSH form (`git@github.com:o/r.git`), `ssh://`, `https://` and `git://`,
/// and rejects any host that merely *contains* "github.com".
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let host = "github.com";
    let mut search_from = 0;
    let index = loop {
        let offset = url[search_from..].find(host)?;
        let found = search_from + offset;
        let boundary_before = match url[..found].chars().next_back() {
            None | Some('/' | '@' | ':') => true,
            Some(_) => false,
        };
        let after = &url[found + host.len()..];
        // The host must be followed by the path separator, not by more of a
        // different host name (`github.company.com`/`notgithub.com`).
        if boundary_before && matches!(after.chars().next(), Some(':' | '/')) {
            break found;
        }
        search_from = found + host.len();
    };
    let rest = &url[index + host.len()..];
    let rest = rest.trim_start_matches([':', '/']).trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// The GitHub remote to use for `dir`, or `None` when no remote reaches
/// github.com (spec decision 33): `remote.pushDefault`, then the current
/// branch's remote, then a remote named `origin`, then one named `github`,
/// then the first remote whose URL is on github.com. Remotes on other hosts
/// are never used, even when they come first.
pub fn resolve_remote(dir: &Path) -> Option<RemoteRef> {
    let remotes = remotes(dir);
    if remotes.is_empty() {
        return None;
    }
    let mut ordered: Vec<&(String, String)> = Vec::new();
    let pinned = config_value(dir, "remote.pushDefault");
    let branch = current_branch(dir).ok();
    let branch_remote = branch
        .as_deref()
        .and_then(|branch| config_value(dir, &format!("branch.{branch}.remote")))
        .filter(|remote| remote != ".");
    for name in [pinned, branch_remote].into_iter().flatten() {
        if let Some(remote) = remotes.iter().find(|(candidate, _)| *candidate == name)
            && !ordered.iter().any(|(existing, _)| *existing == remote.0)
        {
            ordered.push(remote);
        }
    }
    for name in ["origin", "github"] {
        if let Some(remote) = remotes.iter().find(|(candidate, _)| candidate == name)
            && !ordered.iter().any(|(existing, _)| *existing == remote.0)
        {
            ordered.push(remote);
        }
    }
    for remote in &remotes {
        if !ordered.iter().any(|(existing, _)| *existing == remote.0) {
            ordered.push(remote);
        }
    }
    ordered.into_iter().find_map(|(name, url)| {
        let (owner, repo) = parse_github_remote(url)?;
        Some(RemoteRef {
            name: name.clone(),
            owner,
            repo,
            url: url.clone(),
        })
    })
}

/// Git's admin directory for `dir`, shared by every worktree of the repo.
/// Relative results are resolved against `dir` because `rev-parse` reports
/// them relative to the process's working directory.
pub fn git_common_dir(dir: &Path) -> anyhow::Result<PathBuf> {
    let raw = run_git(dir, &["rev-parse", "--git-common-dir"])?;
    if raw.is_empty() {
        anyhow::bail!("git could not report the common directory of {}", dir.display());
    }
    let path = Path::new(&raw);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        dir.join(path)
    })
}

/// Make `worktrees/` invisible to git by appending it to the shared
/// `info/exclude` (spec decision 18). Never the tracked `.gitignore`, so the
/// exclusion cannot dirty the tree the branch guard checks. Idempotent.
pub fn ensure_excluded(repo_dir: &Path) -> anyhow::Result<()> {
    let exclude = git_common_dir(repo_dir)?.join("info").join("exclude");
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("could not create {}: {e}", parent.display()))?;
    }
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    let already = existing.lines().any(|line| {
        line.trim().trim_end_matches('/') == WORKTREE_IGNORE_ENTRY.trim_end_matches('/')
    });
    if already {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(WORKTREE_IGNORE_ENTRY);
    text.push('\n');
    std::fs::write(&exclude, text)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", exclude.display()))?;
    Ok(())
}

/// Add a worktree at `path` on a new `branch` cut from `base`, excluded from
/// git's view first so the main checkout never shows the new files.
pub fn worktree_add(
    repo_dir: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> anyhow::Result<()> {
    ensure_excluded(repo_dir)?;
    let path = path.to_string_lossy().to_string();
    run_git(repo_dir, &["worktree", "add", &path, "-b", branch, base]).map(|_| ())
}

/// Remove a worktree, refusing when it holds uncommitted work so the user is
/// never silently deleted out of an edit. The branch survives (verified: git
/// keeps it), which is what keeps it in the "Branches to clean up" list.
pub fn worktree_remove(repo_dir: &Path, path: &Path) -> anyhow::Result<()> {
    if path.is_dir() {
        let changed = changed_paths(path)?;
        if !changed.is_empty() {
            let listed: Vec<&str> = changed.iter().take(5).map(String::as_str).collect();
            anyhow::bail!(
                "the worktree has uncommitted changes: {}",
                listed.join(", ")
            );
        }
    }
    let path = path.to_string_lossy().to_string();
    run_git(repo_dir, &["worktree", "remove", &path]).map(|_| ())
}

/// Push `branch` to `remote` and set its upstream. Auth is the user's own git
/// credentials (decision 30): the API token never reaches git, a remote URL,
/// or a credential helper. A failure carries git's own stderr, which is what
/// the step blocks with.
pub fn push_branch(dir: &Path, remote: &str, branch: &str) -> anyhow::Result<()> {
    run_git(dir, &["push", "-u", remote, branch]).map(|_| ())
}

/// Stage everything in `dir` and commit it, for the PR step's "Commit and
/// continue" (§6.7).
pub fn stage_and_commit(dir: &Path, message: &str) -> anyhow::Result<()> {
    run_git(dir, &["add", "-A"])?;
    run_git(dir, &["commit", "-q", "-m", message]).map(|_| ())
}

/// Forget admin entries whose checkout was deleted by hand (`rm -rf`), after
/// which `git worktree list` reports them as prunable.
pub fn worktree_prune(repo_dir: &Path) -> anyhow::Result<()> {
    run_git(repo_dir, &["worktree", "prune"]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway repo with one commit on `main`, plus its temp directory so
    /// the caller can clean up.
    struct TempRepo {
        dir: std::path::PathBuf,
    }

    impl TempRepo {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "todo2-coding-git-{name}-{}",
                std::process::id()
            ));
            if dir.exists() {
                std::fs::remove_dir_all(&dir).expect("clean temp dir");
            }
            std::fs::create_dir_all(&dir).expect("create temp dir");
            let repo = TempRepo { dir };
            repo.git(&["init", "-q", "-b", "main"]);
            repo.commit("init");
            repo
        }

        fn git(&self, args: &[&str]) -> String {
            run_git(&self.dir, args).expect("git command")
        }

        fn write(&self, name: &str, contents: &str) {
            std::fs::write(self.dir.join(name), contents).expect("write file");
        }

        /// Commit everything currently staged/untracked with an identity, so
        /// the tests do not depend on the machine's git config.
        fn commit(&self, message: &str) {
            self.git(&["add", "."]);
            self.git(&[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ]);
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn creates_and_deletes_a_branch() {
        let repo = TempRepo::new("branch");
        assert!(is_repo(&repo.dir));
        repo.git(&["switch", "-c", "feature/first"]);
        assert!(branch_exists(&repo.dir, "feature/first"));
        repo.git(&["switch", "main"]);
        assert_eq!(current_branch(&repo.dir).unwrap(), "main");
        delete_branch(&repo.dir, "feature/first").expect("delete");
        assert!(!branch_exists(&repo.dir, "feature/first"));
    }

    #[test]
    fn reports_a_dirty_tree() {
        let repo = TempRepo::new("dirty");
        assert!(changed_paths(&repo.dir).unwrap().is_empty());
        repo.write("new.txt", "hello");
        let changed = changed_paths(&repo.dir).unwrap();
        assert_eq!(changed.len(), 1, "untracked files count as changes");
        assert!(changed[0].ends_with("new.txt"));
    }

    #[test]
    fn merges_a_feature_branch() {
        let repo = TempRepo::new("merge");
        repo.git(&["switch", "-c", "feature/x"]);
        repo.write("feature.txt", "work");
        repo.commit("work");
        switch_branch(&repo.dir, "main").expect("switch back");
        merge_no_ff(&repo.dir, "feature/x").expect("merge");
        assert!(repo.dir.join("feature.txt").exists());
    }

    #[test]
    fn aborts_a_conflicting_merge() {
        let repo = TempRepo::new("conflict");
        repo.write("shared.txt", "base");
        repo.commit("base");
        repo.git(&["switch", "-c", "feature/x"]);
        repo.write("shared.txt", "feature");
        repo.commit("feature");
        switch_branch(&repo.dir, "main").expect("switch back");
        repo.write("shared.txt", "main");
        repo.commit("main");
        assert!(merge_no_ff(&repo.dir, "feature/x").is_err());
        abort_merge(&repo.dir).expect("abort");
        // The abort leaves a clean tree we can keep working from.
        assert!(changed_paths(&repo.dir).unwrap().is_empty());
    }

    #[test]
    fn parses_the_github_remote_forms_and_rejects_other_hosts() {
        for url in [
            "git@github.com:lofi-tools/taskstream.git",
            "https://github.com/lofi-tools/taskstream.git",
            "https://github.com/lofi-tools/taskstream",
            "ssh://git@github.com/lofi-tools/taskstream.git",
            "git://github.com/lofi-tools/taskstream.git",
            "github.com/lofi-tools/taskstream",
        ] {
            assert_eq!(
                parse_github_remote(url),
                Some(("lofi-tools".to_string(), "taskstream".to_string())),
                "{url}"
            );
        }
        // Other hosts, lookalike hosts and shapeless paths resolve to nothing.
        assert!(parse_github_remote("git@gitlab.com:lofi-tools/taskstream.git").is_none());
        assert!(parse_github_remote("https://notgithub.com/o/r").is_none());
        assert!(parse_github_remote("https://github.company.com/o/r").is_none());
        assert!(parse_github_remote("git@github.com:only-owner.git").is_none());
        assert!(parse_github_remote("git@github.com:owner/group/repo.git").is_none());
        assert!(parse_github_remote("/tmp/local-repo").is_none());
    }

    /// The fixture that forced decision 33: two remotes, neither named
    /// `origin`, and `remote.pushDefault` picking the GitHub one.
    #[test]
    fn resolves_the_github_remote_even_without_an_origin() {
        let repo = TempRepo::new("remote-pushdefault");
        repo.git(&["remote", "add", "github", "git@github.com:lofi-tools/taskstream.git"]);
        repo.git(&["remote", "add", "gitlab", "git@gitlab.com:lofi-tools/taskstream.git"]);
        repo.git(&["config", "remote.pushDefault", "github"]);

        let resolved = resolve_remote(&repo.dir).expect("github remote");
        assert_eq!(resolved.name, "github");
        assert_eq!(resolved.owner, "lofi-tools");
        assert_eq!(resolved.repo, "taskstream");
        // A GitLab remote alone never resolves to a GitHub remote.
        let gitlab_only = TempRepo::new("remote-gitlab");
        gitlab_only.git(&["remote", "add", "gitlab", "git@gitlab.com:o/r.git"]);
        assert!(resolve_remote(&gitlab_only.dir).is_none());
    }

    #[test]
    fn resolves_origin_before_the_rest_and_the_branchs_remote_first() {
        let repo = TempRepo::new("remote-order");
        repo.git(&["remote", "add", "upstream", "git@github.com:upstream/api.git"]);
        repo.git(&["remote", "add", "origin", "git@github.com:me/api.git"]);
        assert_eq!(resolve_remote(&repo.dir).unwrap().name, "origin");

        // The current branch's own remote outranks `origin`.
        repo.git(&["switch", "-c", "feature/x"]);
        repo.git(&["config", "branch.feature/x.remote", "upstream"]);
        assert_eq!(resolve_remote(&repo.dir).unwrap().name, "upstream");

        // No github.com remote at all: local merge territory (decision 17).
        let bare = TempRepo::new("remote-none");
        bare.git(&["remote", "add", "origin", "/tmp/elsewhere.git"]);
        assert!(resolve_remote(&bare.dir).is_none());
    }

    #[test]
    fn excludes_worktrees_inside_the_worktree_itself() {
        let repo = TempRepo::new("worktree-exclude");
        let worktree = repo.dir.join("worktrees").join("123-add-login");
        worktree_add(&repo.dir, &worktree, "feature/123-add-login", "main").unwrap();
        assert!(worktree.is_dir());
        assert!(branch_exists(&repo.dir, "feature/123-add-login"));
        // The main checkout shows nothing: `worktrees/` is excluded, so it
        // can never be committed and the branch guard never trips on it.
        assert!(changed_paths(&repo.dir).unwrap().is_empty());
        let exclude = git_common_dir(&repo.dir)
            .unwrap()
            .join("info")
            .join("exclude");
        let written = std::fs::read_to_string(&exclude).unwrap();
        assert_eq!(
            written
                .lines()
                .filter(|line| line.trim() == "worktrees/")
                .count(),
            1
        );

        // Idempotent: a second worktree does not append the entry again.
        let second = repo.dir.join("worktrees").join("124-second");
        worktree_add(&repo.dir, &second, "feature/124-second", "main").unwrap();
        assert!(second.is_dir());
        let written = std::fs::read_to_string(&exclude).unwrap();
        assert_eq!(
            written
                .lines()
                .filter(|line| line.trim() == "worktrees/")
                .count(),
            1
        );

        // Removal keeps the branch (decision 28).
        worktree_remove(&repo.dir, &worktree).unwrap();
        assert!(!worktree.exists());
        assert!(branch_exists(&repo.dir, "feature/123-add-login"));
    }

    #[test]
    fn a_dirty_worktree_refuses_removal() {
        let repo = TempRepo::new("worktree-dirty");
        let worktree = repo.dir.join("worktrees").join("dirty");
        worktree_add(&repo.dir, &worktree, "feature/dirty", "main").unwrap();
        std::fs::write(worktree.join("uncommitted.txt"), "wip").unwrap();

        let error = worktree_remove(&repo.dir, &worktree).unwrap_err().to_string();
        assert!(error.contains("uncommitted.txt"), "{error}");
        assert!(worktree.is_dir(), "a refused removal leaves the checkout");

        std::fs::remove_file(worktree.join("uncommitted.txt")).unwrap();
        worktree_remove(&repo.dir, &worktree).unwrap();
        assert!(!worktree.exists());
    }

    #[test]
    fn stages_and_commits_a_dirty_worktree() {
        let repo = TempRepo::new("worktree-commit");
        // Worktrees share the repo's config, so one identity covers both.
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        let worktree = repo.dir.join("worktrees").join("commit");
        worktree_add(&repo.dir, &worktree, "feature/commit", "main").unwrap();
        std::fs::write(worktree.join("work.txt"), "wip").unwrap();
        assert!(!changed_paths(&worktree).unwrap().is_empty());

        stage_and_commit(&worktree, "Add the change").expect("commit");
        assert!(changed_paths(&worktree).unwrap().is_empty());
        // The commit landed on the worktree's own branch.
        let subject = run_git(&worktree, &["log", "-1", "--pretty=%s"]).unwrap();
        assert_eq!(subject, "Add the change");
    }

    #[test]
    fn pushes_a_branch_to_the_remote_it_was_resolved_from() {
        let repo = TempRepo::new("push");
        let bare = std::env::temp_dir().join(format!(
            "todo2-coding-git-bare-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&bare).ok();
        std::fs::create_dir_all(&bare).expect("create bare repo");
        run_git(&bare, &["init", "-q", "--bare"]).expect("init bare");
        repo.git(&["remote", "add", "github", &bare.to_string_lossy()]);
        let worktree = repo.dir.join("worktrees").join("push");
        worktree_add(&repo.dir, &worktree, "feature/push", "main").unwrap();

        push_branch(&worktree, "github", "feature/push").expect("push");
        let refs = run_git(&bare, &["branch", "--list"]).unwrap();
        assert!(refs.contains("feature/push"), "{refs}");
        // The remote is a local path, so it is not a github.com remote and
        // must never be used for a pull request (decision 33).
        assert!(resolve_remote(&repo.dir).is_none());

        std::fs::remove_dir_all(&bare).ok();
    }

    #[test]
    fn prune_forgets_a_checkout_deleted_by_hand() {
        let repo = TempRepo::new("worktree-prune");
        let worktree = repo.dir.join("worktrees").join("gone");
        worktree_add(&repo.dir, &worktree, "feature/gone", "main").unwrap();
        std::fs::remove_dir_all(&worktree).unwrap();

        // The stale admin entry is still registered until pruned.
        let listed = repo.git(&["worktree", "list", "--porcelain"]);
        assert!(listed.contains(&worktree.to_string_lossy().to_string()));
        worktree_prune(&repo.dir).unwrap();
        let listed = repo.git(&["worktree", "list", "--porcelain"]);
        assert!(!listed.contains(&worktree.to_string_lossy().to_string()));
    }

    #[test]
    fn rebases_a_branch_onto_a_moved_base() {
        let repo = TempRepo::new("rebase");
        repo.write("shared.txt", "base");
        repo.commit("base");
        repo.git(&["switch", "-c", "feature/x"]);
        repo.write("feature.txt", "work");
        repo.commit("work");
        // Base moves on while the branch is being worked on.
        repo.git(&["switch", "main"]);
        repo.write("main.txt", "elsewhere");
        repo.commit("elsewhere");
        repo.git(&["switch", "feature/x"]);

        rebase_onto(&repo.dir, "main").expect("rebase");
        // The branch sits *on top of* the moved base: its parent commit is the
        // base's tip, and the base's own work is now in the branch's tree.
        assert_eq!(
            repo.git(&["rev-parse", "HEAD~1"]),
            repo.git(&["rev-parse", "main"])
        );
        assert!(repo.dir.join("main.txt").exists());
        assert!(repo.dir.join("feature.txt").exists());
        assert!(changed_paths(&repo.dir).unwrap().is_empty());
    }

    #[test]
    fn aborts_a_conflicting_rebase_and_leaves_the_branch_where_it_was() {
        let repo = TempRepo::new("rebase-conflict");
        repo.write("shared.txt", "base");
        repo.commit("base");
        repo.git(&["switch", "-c", "feature/x"]);
        repo.write("shared.txt", "feature");
        repo.commit("feature");
        let before = repo.git(&["rev-parse", "HEAD"]);
        repo.git(&["switch", "main"]);
        repo.write("shared.txt", "main");
        repo.commit("main");
        repo.git(&["switch", "feature/x"]);

        assert!(rebase_onto(&repo.dir, "main").is_err());
        rebase_abort(&repo.dir).expect("abort");
        // Aborting restores the branch exactly: same head and a clean tree,
        // with no conflict markers left for the agent pane to trip over.
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), before);
        assert!(changed_paths(&repo.dir).unwrap().is_empty());
        assert!(!repo.git(&["status", "--porcelain"]).contains("rebase"));
    }

    #[test]
    fn predicts_a_conflict_without_touching_the_worktree() {
        let repo = TempRepo::new("merge-tree");
        repo.write("shared.txt", "base");
        repo.commit("base");
        repo.git(&["switch", "-c", "feature/x"]);
        repo.write("shared.txt", "feature");
        repo.commit("feature");
        repo.git(&["switch", "main"]);
        repo.write("shared.txt", "main");
        repo.commit("main");

        // Both sides changed the same file: named, and nothing was applied —
        // the preview is a read, so it is safe to run while an agent works.
        let paths = merge_tree_conflicts(&repo.dir, "main", "feature/x").unwrap();
        assert_eq!(paths, vec!["shared.txt".to_string()]);
        assert_eq!(current_branch(&repo.dir).unwrap(), "main");
        assert!(changed_paths(&repo.dir).unwrap().is_empty());

        // A branch that only adds a file would merge cleanly.
        repo.git(&["switch", "-c", "feature/clean", "main"]);
        repo.write("clean.txt", "new");
        repo.commit("new");
        assert!(
            merge_tree_conflicts(&repo.dir, "main", "feature/clean")
                .unwrap()
                .is_empty()
        );

        // A ref that does not exist is an error, never a silent "no
        // conflicts": reporting a clean merge for a typo would be a lie.
        assert!(merge_tree_conflicts(&repo.dir, "main", "feature/absent").is_err());
    }

    #[test]
    fn records_the_run_that_owns_a_branch_in_the_repo() {
        let repo = TempRepo::new("managed");
        repo.git(&["switch", "-c", "feature/x"]);
        assert_eq!(managed_run_id(&repo.dir, "feature/x"), None);

        mark_branch_managed(&repo.dir, "feature/x", 42).expect("mark");
        assert_eq!(managed_run_id(&repo.dir, "feature/x"), Some(42));
        // The claim belongs to the repo, not to the checked-out branch.
        repo.git(&["switch", "main"]);
        assert_eq!(managed_run_id(&repo.dir, "feature/x"), Some(42));
        assert_eq!(managed_run_id(&repo.dir, "main"), None);

        // Clearing is idempotent, so deleting an unclaimed branch is fine.
        unmark_branch_managed(&repo.dir, "feature/x").expect("unmark");
        unmark_branch_managed(&repo.dir, "feature/x").expect("unmark twice");
        assert_eq!(managed_run_id(&repo.dir, "feature/x"), None);
    }
}
