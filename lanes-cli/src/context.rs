//! Safer, scripted edits to a context's own files, instead of hand-editing
//! them directly. See DECISIONS.md, "Display names stay denormalized," for
//! why this edits the same few files a human would (lane.toml,
//! zellij-tabs.toml, board.yaml) rather than inventing a single synced
//! "name" concept across them.

use std::io;
use std::path::Path;

use crate::config;

/// Sets a context's display name everywhere it's actually shown today:
/// `lane.toml`'s `name` (if this context is a lane), `zellij-tabs.toml`'s
/// `name` for its terminal session (if it has one), and its `board.yaml`
/// `title` (if this context has a `board.toml`). Each file is independent -
/// a context missing one of them (e.g. a board-only context with no
/// `lane.toml`) just skips that step, not an error. Returns which files
/// were actually touched, for the caller to report; an empty result (none
/// of the three applied) is itself an error, since that means `id` isn't a
/// real context at all.
pub fn set_display_name(cfg: &config::Config, id: &str, name: &str) -> Result<Vec<&'static str>, String> {
    let Some(ctx) = cfg.contexts.iter().find(|c| c.id == id) else {
        return Err(format!("context '{id}' has no lane.toml or board.toml - nothing to rename"));
    };

    let mut touched = Vec::new();

    if let Some(lane) = &ctx.lane {
        set_lane_name(id, name).map_err(|e| format!("lane.toml: {e}"))?;
        touched.push("lane.toml");

        if let Some(session) = lane.terminal_session() {
            match set_ztabs_name(session, name) {
                Ok(true) => touched.push("zellij-tabs.toml"),
                Ok(false) => {}
                Err(e) => return Err(format!("zellij-tabs.toml: {e}")),
            }
        }
    }

    if let Some(board) = &ctx.board {
        set_board_yaml_title(&crate::boards_data_dir(), &board.project, name).map_err(|e| format!("board.yaml: {e}"))?;
        touched.push("board.yaml");
    }

    if touched.is_empty() {
        return Err(format!("context '{id}' has no lane.toml or board.toml - nothing to rename"));
    }

    Ok(touched)
}

/// Same write-then-rename pattern as `config::set_lane_active` - toml_edit
/// so the rest of the file (formatting, key order) is left untouched.
fn set_lane_name(lane_id: &str, name: &str) -> io::Result<()> {
    let path = config::config_dir().join("contexts").join(lane_id).join("lane.toml");
    let mut doc = std::fs::read_to_string(&path)?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    doc["name"] = toml_edit::value(name);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, &path)
}

/// Sets `name` on the `[[tabs]]` entry of `~/.config/infra/zellij-tabs.toml`
/// whose `session` matches - see `set_ztabs_name_at` for the injectable-
/// path version this wraps (a test seam, same reason as
/// `engine::run_with_program` elsewhere in this codebase).
fn set_ztabs_name(session: &str, name: &str) -> io::Result<bool> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = Path::new(&home).join(".config/infra/zellij-tabs.toml");
    set_ztabs_name_at(&path, session, name)
}

/// Via toml_edit for the same untouched-formatting reason as
/// `set_lane_name`. `Ok(false)` (not an error) when there's no `[[tabs]]`
/// array at all, or no entry for this session - ztabs tracking a lane's
/// session is optional, so a lane without one just has nothing here to
/// update.
fn set_ztabs_name_at(path: &Path, session: &str, name: &str) -> io::Result<bool> {
    let text = std::fs::read_to_string(path)?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let Some(tabs) = doc.get_mut("tabs").and_then(|t| t.as_array_of_tables_mut()) else {
        return Ok(false);
    };
    let Some(tab) = tabs.iter_mut().find(|t| t.get("session").and_then(|v| v.as_str()) == Some(session)) else {
        return Ok(false);
    };
    tab["name"] = toml_edit::value(name);
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// The same shape Boards' own `board.rs::BoardMeta` uses - kept in sync
/// deliberately (see `boards_data_dir`'s doc comment on why this crate
/// duplicates rather than links against Boards).
#[derive(serde::Deserialize, serde::Serialize)]
struct BoardMeta {
    title: String,
    columns: Vec<String>,
}

/// Rewrites `<data_dir>/<project>/board.yaml`'s `title`, round-tripped
/// through `BoardMeta` via `noyalib` (the crate Boards itself uses for this
/// file) so `columns` survives unchanged. Not comment-preserving - noyalib
/// has no CST-editing API for this the way toml_edit does for TOML - but
/// Boards only ever writes this file in this exact machine-generated shape
/// itself, so there's nothing to lose today.
fn set_board_yaml_title(data_dir: &Path, project: &str, title: &str) -> io::Result<()> {
    let path = data_dir.join(project).join("board.yaml");
    let text = std::fs::read_to_string(&path)?;
    let mut meta: BoardMeta =
        noyalib::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    meta.title = title.to_string();
    let content = noyalib::to_string(&meta).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lanes-context-test-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn set_board_yaml_title_rewrites_title_and_keeps_columns() {
        let dir = tmp_dir("board-yaml");
        std::fs::create_dir_all(dir.join("infra-board")).unwrap();
        std::fs::write(dir.join("infra-board/board.yaml"), "title: \"Infra\"\ncolumns:\n  - Inbox\n  - Now\n").unwrap();

        set_board_yaml_title(&dir, "infra-board", "Renamed").unwrap();

        let meta: BoardMeta = noyalib::from_str(&std::fs::read_to_string(dir.join("infra-board/board.yaml")).unwrap()).unwrap();
        assert_eq!(meta.title, "Renamed");
        assert_eq!(meta.columns, vec!["Inbox".to_string(), "Now".to_string()]);
    }

    #[test]
    fn set_ztabs_name_updates_the_matching_session_only() {
        let dir = tmp_dir("ztabs");
        let path = dir.join("zellij-tabs.toml");
        std::fs::write(
            &path,
            "[[tabs]]\nname = \"Infra\"\nsession = \"infra\"\n\n[[tabs]]\nname = \"Lanes\"\nsession = \"lanes\"\n",
        )
        .unwrap();

        let updated = set_ztabs_name_at(&path, "infra", "Renamed").unwrap();
        assert!(updated);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("name = \"Renamed\"\nsession = \"infra\""));
        assert!(text.contains("name = \"Lanes\"\nsession = \"lanes\""));
    }

    #[test]
    fn set_ztabs_name_false_when_no_entry_matches() {
        let dir = tmp_dir("ztabs-no-match");
        let path = dir.join("zellij-tabs.toml");
        std::fs::write(&path, "[[tabs]]\nname = \"Infra\"\nsession = \"infra\"\n").unwrap();

        let updated = set_ztabs_name_at(&path, "nonexistent", "Renamed").unwrap();
        assert!(!updated);
    }

}
