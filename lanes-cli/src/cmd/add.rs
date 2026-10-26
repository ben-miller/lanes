//! `lanes ctx <id> add`: scaffold a context's facets - the CRUD primitive
//! that was missing this whole time (`set-name` and the `lane`/`tab`/
//! `board` `enable`/`disable` pairs only ever operate on a facet that
//! already exists). Works for a brand-new context (directory doesn't
//! exist yet) and equally for adding a facet an *existing* context never
//! had (e.g. giving a lane-only context a board too) - the only thing
//! this ever refuses is a facet that's already there, never the context
//! existing at all. Writes whichever facet files are requested, with
//! everything defaulting to enabled (see DECISIONS.md's "disabled means
//! absent" design - a freshly created facet has no reason to start
//! disabled), appends the id to `contexts.toml`'s `order` (a no-op if
//! it's already listed), and (for `--board`) materializes its
//! `board.yaml` immediately via the same path `lanes doctor --fix` uses,
//! so the context passes every `doctor` check right away instead of
//! needing a follow-up `--fix` run.

use std::io;

pub struct Options {
    pub lane: bool,
    pub board: bool,
    pub project: Option<String>,
    pub session: Option<String>,
    pub repo: Option<String>,
}

pub fn run(id: &str, name: &str, opts: Options) {
    if let Err(e) = validate(id, &opts) {
        eprintln!("{e}");
        std::process::exit(1);
    }

    let dir = lanes::config::config_dir().join("contexts").join(id);
    if let Err(e) = check_against_existing_facets(id, &dir, &opts) {
        eprintln!("{e}");
        std::process::exit(1);
    }

    if let Err(e) = scaffold(&dir, name, &opts) {
        eprintln!("{e}");
        std::process::exit(1);
    }

    if let Err(e) = lanes::config::append_context_order(id) {
        eprintln!("added to {id:?} but failed to register it in contexts.toml's order: {e}");
        std::process::exit(1);
    }

    let mut created = Vec::new();
    if opts.lane {
        created.push("lane.toml".to_string());
    }
    if opts.session.is_some() || opts.repo.is_some() {
        created.push("tab.toml".to_string());
    }
    if opts.board {
        created.push("board.toml".to_string());
        let cfg = lanes::config::Config::load();
        let board_yaml_created = super::doctor::materialize_boards_projects(&cfg, &lanes::boards_data_dir());
        created.extend(board_yaml_created.into_iter().map(|p| format!("board.yaml for {p:?}")));
    }
    println!("{id}: {}", created.join(", "));
}

/// Rejects a request before anything touches the filesystem: `id` must be
/// kebab-case (same rule `doctor`'s "session naming" check enforces, but
/// checked up front here instead of letting a bad id land and get caught
/// later), and at least one facet must actually be requested.
/// `--session`/`--repo` populate `tab.toml`'s scope, read only when a lane
/// facet exists at all (see `load_contexts`) - whether that means "pass
/// --lane too" depends on whether the context already has one, which
/// `check_against_existing_facets` (filesystem-aware) decides, not this.
fn validate(id: &str, opts: &Options) -> Result<(), String> {
    if !super::doctor::is_kebab_case(id) {
        return Err(format!("{id:?} is not kebab-case (lowercase letters, digits, single hyphens)"));
    }
    if !opts.lane && !opts.board && opts.session.is_none() && opts.repo.is_none() {
        return Err("nothing to create - pass --lane, --board, --session, and/or --repo".to_string());
    }
    Ok(())
}

/// Refuses to clobber a facet that's already there (the only thing that
/// blocks `add`, never the context existing at all - see this module's
/// own doc comment), and makes sure `--session`/`--repo` have an actual
/// lane facet to attach to: either `--lane` is creating one right now, or
/// the context already has one.
fn check_against_existing_facets(id: &str, dir: &std::path::Path, opts: &Options) -> Result<(), String> {
    let lane_exists = dir.join("lane.toml").exists();
    if opts.lane && lane_exists {
        return Err(format!("{id:?} already has a lane facet"));
    }
    if opts.board && dir.join("board.toml").exists() {
        return Err(format!("{id:?} already has a board facet"));
    }
    if (opts.session.is_some() || opts.repo.is_some()) && !opts.lane && !lane_exists {
        return Err(
            "--session/--repo populate tab.toml, which needs a lane facet - pass --lane to create one here too"
                .to_string(),
        );
    }
    Ok(())
}

fn scaffold(dir: &std::path::Path, name: &str, opts: &Options) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;

    if opts.lane {
        write_lane_toml(dir, name)?;
    }
    if opts.session.is_some() || opts.repo.is_some() {
        write_tab_toml(dir, opts.session.as_deref(), opts.repo.as_deref())?;
    }

    if opts.board {
        let project = opts.project.clone().unwrap_or_else(|| name.to_string());
        write_board_toml(dir, &project)?;
    }

    Ok(())
}

fn write_lane_toml(dir: &std::path::Path, name: &str) -> io::Result<()> {
    let mut doc = toml_edit::DocumentMut::new();
    doc["name"] = toml_edit::value(name);
    std::fs::write(dir.join("lane.toml"), doc.to_string())
}

fn write_tab_toml(dir: &std::path::Path, session: Option<&str>, repo: Option<&str>) -> io::Result<()> {
    let mut doc = toml_edit::DocumentMut::new();
    let mut scope = toml_edit::ArrayOfTables::new();
    if let Some(session) = session {
        let mut t = toml_edit::Table::new();
        t["kind"] = toml_edit::value("zellij_session");
        t["session"] = toml_edit::value(session);
        scope.push(t);
    }
    if let Some(repo) = repo {
        let mut t = toml_edit::Table::new();
        t["kind"] = toml_edit::value("repo");
        t["path"] = toml_edit::value(repo);
        scope.push(t);
    }
    doc["scope"] = toml_edit::Item::ArrayOfTables(scope);
    std::fs::write(dir.join("tab.toml"), doc.to_string())
}

/// Same placement template every hand-authored board.toml in this config
/// already uses (full-screen on `main`, raised, shrinking to a 3/4-width
/// column when solo) - see DECISIONS.md, "Boards target."
fn write_board_toml(dir: &std::path::Path, project: &str) -> io::Result<()> {
    let mut doc = toml_edit::DocumentMut::new();
    doc["driver"] = toml_edit::value("boards");
    doc["project"] = toml_edit::value(project);
    doc["monitor"] = toml_edit::value("main");
    doc["position"] = toml_edit::value("full");
    doc["raise"] = toml_edit::value(true);
    let mut position_solo = toml_edit::InlineTable::new();
    position_solo.insert("cols", 4.into());
    let mut col = toml_edit::Array::new();
    col.push(1);
    col.push(3);
    position_solo.insert("col", toml_edit::Value::Array(col));
    doc["position_solo"] = toml_edit::Item::Value(toml_edit::Value::InlineTable(position_solo));
    std::fs::write(dir.join("board.toml"), doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_fake_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("lanes-add-test-home-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config/lanes/contexts")).unwrap();
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", &home);
        let result = f(&home);
        match prev {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&home).ok();
        result
    }

    fn opts() -> Options {
        Options { lane: false, board: false, project: None, session: None, repo: None }
    }

    #[test]
    fn validate_rejects_a_non_kebab_case_id() {
        assert!(validate("Not Kebab", &Options { lane: true, ..opts() }).is_err());
    }

    #[test]
    fn validate_rejects_no_facets_requested() {
        assert!(validate("ctx", &opts()).is_err());
    }

    #[test]
    fn validate_accepts_bare_session_with_no_lane_or_board_flag() {
        // --session alone is a real request ("add a tab facet to an
        // existing lane") - validate can't know yet whether a lane facet
        // exists, only that this isn't an empty request.
        assert!(validate("ctx", &Options { session: Some("ctx".to_string()), ..opts() }).is_ok());
    }

    #[test]
    fn validate_accepts_session_without_lane_up_front() {
        // Whether this is actually OK depends on whether a lane facet
        // already exists on disk - validate() itself can't know that, so
        // it defers to check_against_existing_facets.
        assert!(validate("ctx", &Options { board: true, session: Some("ctx".to_string()), ..opts() }).is_ok());
    }

    #[test]
    fn check_against_existing_facets_allows_adding_board_to_a_lane_only_context() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lane.toml"), "name = \"Ctx\"\n").unwrap();
            assert!(check_against_existing_facets("ctx", &dir, &Options { board: true, ..opts() }).is_ok());
        });
    }

    #[test]
    fn check_against_existing_facets_rejects_a_facet_that_is_already_there() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("board.toml"), "driver = \"boards\"\nproject = \"Ctx\"\n").unwrap();
            assert!(check_against_existing_facets("ctx", &dir, &Options { board: true, ..opts() }).is_err());
        });
    }

    #[test]
    fn check_against_existing_facets_rejects_session_with_no_lane_facet_at_all() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            let opts = Options { board: true, session: Some("ctx".to_string()), ..opts() };
            assert!(check_against_existing_facets("ctx", &dir, &opts).is_err());
        });
    }

    #[test]
    fn check_against_existing_facets_allows_session_against_an_existing_lane_without_the_lane_flag() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lane.toml"), "name = \"Ctx\"\n").unwrap();
            let opts = Options { board: true, session: Some("ctx".to_string()), ..opts() };
            assert!(check_against_existing_facets("ctx", &dir, &opts).is_ok());
        });
    }

    #[test]
    fn scaffold_lane_only_writes_just_lane_toml() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            scaffold(&dir, "Ctx", &Options { lane: true, ..opts() }).unwrap();
            assert!(dir.join("lane.toml").is_file());
            assert!(!dir.join("tab.toml").exists());
            assert!(!dir.join("board.toml").exists());
            let text = std::fs::read_to_string(dir.join("lane.toml")).unwrap();
            assert!(text.contains("name = \"Ctx\""));
        });
    }

    #[test]
    fn scaffold_lane_with_session_and_repo_writes_both_scope_entries() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            scaffold(
                &dir,
                "Ctx",
                &Options { lane: true, session: Some("ctx".to_string()), repo: Some("~/src/ctx".to_string()), ..opts() },
            )
            .unwrap();
            let text = std::fs::read_to_string(dir.join("tab.toml")).unwrap();
            assert!(text.contains("kind = \"zellij_session\""));
            assert!(text.contains("session = \"ctx\""));
            assert!(text.contains("kind = \"repo\""));
            assert!(text.contains("path = \"~/src/ctx\""));
        });
    }

    #[test]
    fn scaffold_board_only_uses_the_name_as_the_default_project() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            scaffold(&dir, "Ctx Board", &Options { board: true, ..opts() }).unwrap();
            assert!(!dir.join("lane.toml").exists());
            let text = std::fs::read_to_string(dir.join("board.toml")).unwrap();
            assert!(text.contains("project = \"Ctx Board\""));
            assert!(text.contains("driver = \"boards\""));
        });
    }

    #[test]
    fn scaffold_board_only_prefers_an_explicit_project_over_the_name() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            scaffold(&dir, "Ctx", &Options { board: true, project: Some("Explicit".to_string()), ..opts() }).unwrap();
            let text = std::fs::read_to_string(dir.join("board.toml")).unwrap();
            assert!(text.contains("project = \"Explicit\""));
        });
    }
}
