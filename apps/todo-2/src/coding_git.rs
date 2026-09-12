//! Git helpers for coding runs. The app owns the feature branch: it is cut
//! once the spec is approved and merged when the merge phase completes.
//!
//! Everything shells out to `git` with an explicit `current_dir` and captures
//! stderr so the details panel can show the user why a phase is blocked (no
//! `git2` dependency, matching the rest of the project). Every function is
//! synchronous and must be called from a blocking context.

use std::path::Path;
use std::process::Command;

/// Run `git <args>` in `dir`, returning trimmed stdout. Failures carry the
/// captured stderr (falling back to stdout when git wrote nothing to stderr)
/// so the message the user sees is the one git produced.
fn run_git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run git: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        anyhow::bail!("git {} failed: {detail}", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

/// Create `branch` from the current HEAD and switch to it.
pub fn create_branch(dir: &Path, branch: &str) -> anyhow::Result<()> {
    run_git(dir, &["switch", "-c", branch]).map(|_| ())
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
}
