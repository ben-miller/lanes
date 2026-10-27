//! Editing a lane's `tab.toml` scope in place: its Zellij session and the
//! repos it works in. Everything goes through here (`ctx <id> add
//! --session/--repo`, `ctx <id> repo add/remove/ls`), and everything reads
//! the existing file first and keeps what it doesn't touch (other scope
//! entries, comments, formatting: `toml_edit`), so adding a repo can never
//! drop the session or another repo.

use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// `~/x` to an absolute path.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

/// How a repo path is written into `tab.toml`: absolute (a relative path is
/// taken from the current directory), symlinks resolved, then `~/...` when
/// it's under the home directory, as the hand-written files have it.
pub fn display(path: &str) -> String {
    let p = expand(path);
    let p = if p.is_relative() { std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p) } else { p };
    let p = p.canonicalize().unwrap_or(p);
    let home = home().canonicalize().unwrap_or_else(|_| home());
    match p.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Whether two repo paths are the same repo, however they're written.
fn same(a: &str, b: &str) -> bool {
    display(a) == display(b)
}

/// A repo to add must be a git repo (a `.git` directory, or a worktree's
/// `.git` file).
fn check_repo(path: &str) -> Result<(), String> {
    let p = expand(path);
    if !p.is_dir() {
        return Err(format!("{path}: no such directory"));
    }
    if !p.join(".git").exists() {
        return Err(format!("{path}: not a git repo (no .git)"));
    }
    Ok(())
}

fn read(dir: &Path) -> Result<DocumentMut, String> {
    let path = dir.join("tab.toml");
    match std::fs::read_to_string(&path) {
        Ok(text) => text.parse::<DocumentMut>().map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Written through a sibling `.tmp` and a rename, so nothing reading it
/// (Lanes Switch, lensd's rules) ever sees half a file.
fn write(dir: &Path, doc: &DocumentMut) -> Result<(), String> {
    let path = dir.join("tab.toml");
    let tmp = dir.join("tab.toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
}

fn scope(doc: &mut DocumentMut) -> &mut ArrayOfTables {
    if !doc.contains_key("scope") || doc["scope"].as_array_of_tables().is_none() {
        doc["scope"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    doc["scope"].as_array_of_tables_mut().expect("just made it one")
}

fn kind(t: &Table) -> Option<&str> {
    t.get("kind").and_then(|k| k.as_str())
}

/// The repos `tab.toml` names, in order, as written.
pub fn repos(dir: &Path) -> Result<Vec<String>, String> {
    let mut doc = read(dir)?;
    Ok(scope(&mut doc)
        .iter()
        .filter(|t| kind(t) == Some("repo"))
        .filter_map(|t| t.get("path").and_then(|p| p.as_str()).map(str::to_string))
        .collect())
}

/// What a merge changed, for the command's output.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub session: Option<String>,
    pub added: Vec<String>,
    pub already: Vec<String>,
}

/// Sets the Zellij session (replacing the one there, if any: a lane has
/// one) and adds each repo not already named, keeping everything else.
/// Every repo is checked first, so a bad one changes nothing.
pub fn merge(dir: &Path, session: Option<&str>, add: &[String]) -> Result<Changes, String> {
    for r in add {
        check_repo(r)?;
    }
    let mut doc = read(dir)?;
    let mut changes = Changes::default();
    let entries = scope(&mut doc);
    if let Some(session) = session {
        let found = entries.iter().position(|t| kind(t) == Some("zellij_session"));
        match found {
            Some(i) => entries.get_mut(i).expect("just found it")["session"] = toml_edit::value(session),
            None => {
                let mut t = Table::new();
                t["kind"] = toml_edit::value("zellij_session");
                t["session"] = toml_edit::value(session);
                entries.push(t);
            }
        }
        changes.session = Some(session.to_string());
    }
    for r in add {
        let shown = display(r);
        let listed = entries.iter().any(|t| kind(t) == Some("repo") && t.get("path").and_then(|p| p.as_str()).is_some_and(|p| same(p, r)));
        if listed || changes.added.contains(&shown) {
            changes.already.push(shown);
            continue;
        }
        let mut t = Table::new();
        t["kind"] = toml_edit::value("repo");
        t["path"] = toml_edit::value(shown.clone());
        entries.push(t);
        changes.added.push(shown);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    write(dir, &doc)?;
    Ok(changes)
}

/// Removes each repo named (however it's written), keeping everything
/// else. Naming one the lane doesn't have is an error, and changes nothing.
pub fn remove(dir: &Path, remove: &[String]) -> Result<Vec<String>, String> {
    let current = repos(dir)?;
    for r in remove {
        if !current.iter().any(|c| same(c, r)) {
            return Err(format!("{}: not one of this lane's repos ({})", display(r), current.join(", ")));
        }
    }
    let mut doc = read(dir)?;
    let entries = scope(&mut doc);
    let mut removed = Vec::new();
    entries.retain(|t| {
        let gone = kind(t) == Some("repo") && t.get("path").and_then(|p| p.as_str()).is_some_and(|p| remove.iter().any(|r| same(p, r)));
        if gone {
            removed.push(t.get("path").and_then(|p| p.as_str()).unwrap_or_default().to_string());
        }
        !gone
    });
    write(dir, &doc)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A lane directory, and git repos to add to it, under a temp HOME
    /// (paths are written relative to it: `~/...`).
    fn setup(name: &str) -> (PathBuf, PathBuf) {
        let home = std::env::temp_dir().join(format!("lanes-scope-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let home = home.canonicalize().unwrap();
        for repo in ["src/a", "src/b"] {
            std::fs::create_dir_all(home.join(repo).join(".git")).unwrap();
        }
        std::fs::create_dir_all(home.join("src/not-a-repo")).unwrap();
        let lane = home.join(".config/lanes/contexts/x");
        std::fs::create_dir_all(&lane).unwrap();
        (home, lane)
    }

    fn with_home<T>(home: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = crate::cmd::HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", home);
        let out = f();
        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    #[test]
    fn adding_a_repo_keeps_the_session_and_the_other_repos() {
        let (home, lane) = setup("keep");
        std::fs::write(lane.join("tab.toml"), "# the lane's anchor\n[[scope]]\nkind = \"zellij_session\"\nsession = \"x\"\n\n[[scope]]\nkind = \"repo\"\npath = \"~/src/a\"\n").unwrap();
        with_home(&home, || {
            let c = merge(&lane, None, &[home.join("src/b").display().to_string()]).unwrap();
            assert_eq!(c.added, ["~/src/b"]);
            let text = std::fs::read_to_string(lane.join("tab.toml")).unwrap();
            assert!(text.contains("# the lane's anchor") && text.contains("session = \"x\""), "kept: {text}");
            assert_eq!(repos(&lane).unwrap(), ["~/src/a", "~/src/b"]);

            // Already there, however it's written: nothing added twice.
            let c = merge(&lane, None, &["~/src/a".into(), home.join("src/b").display().to_string()]).unwrap();
            assert_eq!((c.added.len(), c.already.len()), (0, 2));
            assert_eq!(repos(&lane).unwrap().len(), 2);
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_session_is_replaced_not_duplicated_and_a_bad_repo_changes_nothing() {
        let (home, lane) = setup("session");
        with_home(&home, || {
            merge(&lane, Some("one"), &["~/src/a".into()]).unwrap();
            merge(&lane, Some("two"), &[]).unwrap();
            let text = std::fs::read_to_string(lane.join("tab.toml")).unwrap();
            assert_eq!(text.matches("zellij_session").count(), 1, "{text}");
            assert!(text.contains("session = \"two\"") && text.contains("~/src/a"));

            let before = std::fs::read_to_string(lane.join("tab.toml")).unwrap();
            assert!(merge(&lane, None, &["~/src/b".into(), "~/src/not-a-repo".into()]).unwrap_err().contains("not a git repo"));
            assert!(merge(&lane, None, &["~/src/nowhere".into()]).unwrap_err().contains("no such directory"));
            assert_eq!(std::fs::read_to_string(lane.join("tab.toml")).unwrap(), before, "unchanged");
        });
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn removing_a_repo_keeps_the_rest() {
        let (home, lane) = setup("remove");
        with_home(&home, || {
            merge(&lane, Some("x"), &["~/src/a".into(), "~/src/b".into()]).unwrap();
            assert_eq!(remove(&lane, &[home.join("src/a").display().to_string()]).unwrap(), ["~/src/a"]);
            assert_eq!(repos(&lane).unwrap(), ["~/src/b"]);
            assert!(std::fs::read_to_string(lane.join("tab.toml")).unwrap().contains("session = \"x\""));
            assert!(remove(&lane, &["~/src/a".into()]).unwrap_err().contains("not one of this lane's repos"));
        });
        let _ = std::fs::remove_dir_all(&home);
    }
}
