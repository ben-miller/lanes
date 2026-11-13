//! Repo state for the dashboard's repo signals (dirty, non-default
//! branch) - PLAN-retire-lensd.md step 5, replacing lensd's
//! `lanes_repo_dirty`/`lanes_repo_branch`. Deliberately narrow: just what
//! those two signals need, not a full status view (dp::sc::RepoStatus
//! also covers REVIEW_POLICY, pending `git pc` messages, ahead/behind,
//! unpushed commits - none of which `lanes.dl` ever used, and pulling in
//! the `dp` crate for this one slice would mean pulling in its unrelated
//! dependencies too, like the checks database). The default-branch half
//! reuses `cmd::doctor`'s `cached_origin_head` - the same fast, local,
//! possibly-stale-until-`git remote set-head`-refreshed read lensd's own
//! git source used - rather than re-deriving it a third time.

use std::process::Command;

pub struct RepoStatus {
    /// Any staged, unstaged, or untracked change at all - same definition
    /// `lanes.dl`'s `dirty(Repo)` used (`git.change` for any kind).
    pub dirty: bool,
    /// The current branch, `None` if HEAD is detached.
    pub branch: Option<String>,
    /// `origin/HEAD`'s branch, from the local, possibly-stale cached ref -
    /// same as lensd's `git.default_branch` fact.
    pub default_branch: Option<String>,
}

pub fn status(path: &str) -> RepoStatus {
    let (dirty, branch) = status_porcelain(path);
    let default_branch = cached_origin_head(path);
    RepoStatus { dirty, branch, default_branch }
}

/// `origin/HEAD`'s branch, read purely locally (fast) via the cached ref -
/// moved here from `cmd::doctor` (which still does the slower, live
/// `git ls-remote` check against this, to catch it going stale) so both
/// the dashboard and that diagnostic share one reader instead of two.
pub fn cached_origin_head(path: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", path, "symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().strip_prefix("origin/").map(str::to_string)
}

fn status_porcelain(path: &str) -> (bool, Option<String>) {
    let Ok(out) = Command::new("git").args(["-C", path, "status", "--porcelain=v2", "--branch"]).output() else {
        return (false, None);
    };
    if !out.status.success() {
        return (false, None);
    }
    parse_porcelain(&String::from_utf8_lossy(&out.stdout))
}

/// `git status --porcelain=v2 --branch`'s own format: `# branch.head
/// <name>` (or `(detached)`) is the only header line this needs; any
/// non-`#` line at all is a changed file, regardless of its own kind
/// (`1`/`2` ordinary/renamed, `u` conflicted, `?` untracked) - matching
/// `lanes.dl`'s `dirty(Repo)` (true for any `git.change`, any `Kind`).
fn parse_porcelain(text: &str) -> (bool, Option<String>) {
    let mut dirty = false;
    let mut branch = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            if rest != "(detached)" {
                branch = Some(rest.to_string());
            }
        } else if !line.starts_with('#') {
            dirty = true;
        }
    }
    (dirty, branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_porcelain_reads_the_branch_head_line() {
        let text = "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n";
        let (dirty, branch) = parse_porcelain(text);
        assert!(!dirty);
        assert_eq!(branch.as_deref(), Some("main"));
    }

    #[test]
    fn parse_porcelain_detached_head_is_no_branch() {
        let text = "# branch.oid abc123\n# branch.head (detached)\n";
        let (_, branch) = parse_porcelain(text);
        assert_eq!(branch, None);
    }

    #[test]
    fn parse_porcelain_any_entry_line_is_dirty() {
        let staged = "# branch.head main\n1 M. N... 100644 100644 100644 abc def src/lib.rs\n";
        assert!(parse_porcelain(staged).0);

        let untracked = "# branch.head main\n? new-file.txt\n";
        assert!(parse_porcelain(untracked).0);

        let conflicted = "# branch.head main\nu UU N... 100644 100644 100644 100644 a b c d both.rs\n";
        assert!(parse_porcelain(conflicted).0);
    }

    #[test]
    fn parse_porcelain_clean_repo_is_not_dirty() {
        let text = "# branch.oid abc123\n# branch.head main\n";
        assert!(!parse_porcelain(text).0);
    }
}
