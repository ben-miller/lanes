//! `lanes ctx <id> remove`: the counterpart to `add`. Defaults to a soft,
//! reversible removal - disabling every facet the context has, the same
//! as running `lane`/`board disable` on each one that exists (disabling
//! the lane facet already hides its `tab.toml`/`layout.toml` along with
//! it), so it behaves as if the context were gone without actually
//! deleting anything. `--delete` instead removes it for real: the whole
//! context directory, and its entry in `contexts.toml`'s `order` - no
//! undo, same as any other filesystem delete.

pub fn run(id: &str, delete: bool) {
    let dir = lanes::config::config_dir().join("contexts").join(id);
    if !dir.exists() {
        eprintln!("{id:?} is not a context - no such directory");
        std::process::exit(1);
    }

    if delete {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            eprintln!("failed to delete {id:?}: {e}");
            std::process::exit(1);
        }
        if let Err(e) = lanes::config::remove_context_order(id) {
            eprintln!("deleted {id:?} but failed to remove it from contexts.toml's order: {e}");
            std::process::exit(1);
        }
        println!("deleted {id}");
        return;
    }

    let mut disabled = Vec::new();
    for (facet, file, disable) in facet_disablers(&dir) {
        if file.exists() {
            if let Err(e) = disable(id) {
                eprintln!("{id:?}: {facet}: {e}");
                std::process::exit(1);
            }
            disabled.push(facet);
        }
    }

    if disabled.is_empty() {
        println!("{id} has no facets to disable");
    } else {
        println!("disabled {id}: {}", disabled.join(", "));
    }
}

/// (facet name, its file, the function that disables it).
type FacetDisabler = (&'static str, std::path::PathBuf, fn(&str) -> std::io::Result<()>);

/// Which facet files this context has, and the function that disables
/// each one - driven entirely by file presence, same as `load_contexts`
/// itself, rather than hardcoding an assumption about which facets a
/// context "should" have. `tab.toml`/`layout.toml` have no disabler of
/// their own anymore - `lane.toml`'s `enabled` already governs the whole
/// lane+tab+layout bundle (see `LaneToml`'s doc comment).
fn facet_disablers(dir: &std::path::Path) -> Vec<FacetDisabler> {
    vec![
        ("lane", dir.join("lane.toml"), |id| lanes::config::set_lane_enabled(id, false)),
        ("board", dir.join("board.toml"), |id| lanes::config::set_board_enabled(id, false)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_fake_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!("lanes-remove-test-home-{}-{:?}", std::process::id(), std::thread::current().id()));
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

    #[test]
    fn facet_disablers_only_lists_facets_that_actually_exist() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("board.toml"), "driver = \"boards\"\nproject = \"Ctx\"\n").unwrap();
            let facets: Vec<&str> = facet_disablers(&dir).into_iter().filter(|(_, file, _)| file.exists()).map(|(f, ..)| f).collect();
            assert_eq!(facets, vec!["board"]);
        });
    }

    #[test]
    fn run_delete_removes_the_directory_and_the_order_entry() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("board.toml"), "driver = \"boards\"\nproject = \"Ctx\"\n").unwrap();
            std::fs::write(home.join(".config/lanes/contexts.toml"), "order = [\"a\", \"ctx\", \"b\"]\n").unwrap();

            run("ctx", true);

            assert!(!dir.exists());
            let text = std::fs::read_to_string(home.join(".config/lanes/contexts.toml")).unwrap();
            assert!(!text.contains("ctx"));
            assert!(text.contains("\"a\""));
            assert!(text.contains("\"b\""));
        });
    }

    #[test]
    fn run_without_delete_disables_every_existing_facet_but_keeps_the_directory() {
        with_fake_home(|home| {
            let dir = home.join(".config/lanes/contexts/ctx");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lane.toml"), "name = \"Ctx\"\n").unwrap();
            std::fs::write(dir.join("board.toml"), "driver = \"boards\"\nproject = \"Ctx\"\n").unwrap();

            run("ctx", false);

            assert!(dir.exists());
            let lane = std::fs::read_to_string(dir.join("lane.toml")).unwrap();
            assert!(lane.contains("enabled = false"));
            let board = std::fs::read_to_string(dir.join("board.toml")).unwrap();
            assert!(board.contains("enabled = false"));
            assert!(!dir.join("tab.toml").exists(), "never had a tab facet, shouldn't gain one");
        });
    }
}
