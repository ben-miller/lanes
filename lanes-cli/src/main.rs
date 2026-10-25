mod cmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "lanes", about = "Context manager for your working environment")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Focus a lane: focus terminal and apply window placement
    Focus {
        /// Lane ID to focus. Defaults to the lane whose zellij session
        /// matches $ZELLIJ_SESSION_NAME if omitted.
        id: Option<String>,
    },

    /// Mark a lane active in its config file
    Activate {
        /// Lane ID to activate. Defaults to $ZELLIJ_SESSION_NAME's lane.
        id: Option<String>,
    },

    /// Mark a lane inactive in its config file
    Deactivate {
        /// Lane ID to deactivate. Defaults to $ZELLIJ_SESSION_NAME's lane.
        id: Option<String>,
    },

    /// Ensure baseline Lanes state exists (currently: the switch-ui/hammerspoon
    /// log files) - idempotent, safe to call from either component's own
    /// startup regardless of which one runs first.
    Init,

    /// Check environment dependencies and configuration
    Doctor {
        /// Materialize any missing Boards project (~/boards/<project>/board.yaml)
        /// named by a context's board.toml. Never touches a project that
        /// already exists - Boards owns that file once created.
        #[arg(long)]
        fix: bool,
    },

    /// List every context - bare ids only. `-l`/`--json` for full detail
    /// (every scope element, every target's placement), including
    /// board-only contexts (a project tracked only in Boards, no lane.toml).
    Ls {
        /// Full detail, not just ids
        #[arg(short = 'l', long)]
        long: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Dump the current lane state as JSON
    Snapshot {
        /// Write output to a file instead of stdout
        #[arg(long)]
        out: Option<String>,
    },

    /// List lanes that have signals requiring attention
    Signals,

    /// Print the currently focused lane ID
    Current {
        /// Print the lane's display name instead of its id
        #[arg(long)]
        name: bool,
    },

    /// Activate whichever target the currently focused lane has for the
    /// given driver (e.g. "obsidian") - for a context-
    /// aware hotkey, not lane switching (that's the unrelated `activate`,
    /// which marks a lane active in its config). Falls back to a plain app
    /// launch when there's no focused lane or no matching target.
    ActivateDriver {
        driver: String,
    },

    /// Manage Claude sessions
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },

    /// Scripted edits to a context's own files - a safer alternative to
    /// hand-editing lane.toml/zellij-tabs.toml/board.yaml directly
    #[command(name = "ctx")]
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },

    /// Cycle through active, reachable lanes directly - independent of
    /// whether anything (Claude or otherwise) is running inside them. A
    /// separate policy from `sessions next`/`prev`, not a replacement.
    LaneCycle {
        #[command(subcommand)]
        command: LaneCycleCommand,
    },

    /// Cycle using whichever policy `cycle_policy` names in the global
    /// lanes.toml (default: Claude sessions, same as `sessions next`/
    /// `prev`) - this is what a cycling hotkey should actually call, so
    /// switching policies means editing config, not rebinding keys.
    Cycle {
        #[command(subcommand)]
        command: CycleCommand,
    },

    /// Manage the Zellij session -> WezTerm tab ID cache
    Tabs {
        #[command(subcommand)]
        command: TabsCommand,
    },

    /// Query Lanes Switch's pin-on-top state
    ///
    /// state.kdl is the source of truth for pin state; the running Tauri
    /// app watches it and applies always-on-top/show/hide whenever it
    /// changes, regardless of who wrote it (Hammerspoon or the app itself).
    IsPinned,

    /// Flip Lanes Switch's pin-on-top state and print the new value.
    ///
    /// See `IsPinned` for how the running app picks this up.
    TogglePinned,

    /// Flip Lanes Switch's edit-mode state and print the new value.
    ///
    /// state.kdl is the source of truth, same as `TogglePinned` - the
    /// running app watches it and shows/arms its edit-mode UI whenever it
    /// changes, regardless of who wrote it (Hammerspoon, the tray menu, or
    /// the app itself via Escape).
    ToggleEditMode,

    /// Flip whether Lanes Switch's dashboard shows inactive lanes, and
    /// print the new value.
    ///
    /// state.kdl is the source of truth, same as `TogglePinned` - the
    /// running app's tray checkbox and dashboard both pick this up whenever
    /// it changes, regardless of who wrote it.
    ToggleShowInactive,

    /// Ask the running Lanes Switch app to show+focus its window, without
    /// changing pin state. See `IsPinned` for the state.kdl watch mechanism.
    ShowSwitch,

    /// Ask the running Lanes Switch app to hide its window, without
    /// changing pin state. See `IsPinned` for the state.kdl watch mechanism.
    HideSwitch,

    /// Print the shell hook for the long-running-command signal, to be
    /// sourced from your shell config:
    ///
    ///   lanes shell-init fish | source        # fish
    ///
    /// The hook writes command-lifecycle records to
    /// ~/.local/state/lanes/shell/ for the (opt-in) `shell` driver to read.
    /// See PLAN-shell-signal.md.
    ShellInit {
        /// Which shell's hook to emit
        #[arg(value_enum)]
        shell: ShellKind,
    },

    /// Manage the Lanes Switch app
    SwitchUi {
        #[command(subcommand)]
        command: ComponentCommand,
    },

    /// Manage the Hammerspoon integration
    Hammerspoon {
        #[command(subcommand)]
        command: ComponentCommand,
    },

    /// Manage the switch-latency diagnostics log (perf.log) - trigger,
    /// actual-switch, and UI-update timestamps for every lane/session
    /// switch. On by default; see `logging::perf`.
    Diagnostics {
        #[command(subcommand)]
        command: DiagnosticsCommand,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ShellKind {
    Fish,
}

#[derive(Subcommand)]
enum DiagnosticsCommand {
    /// Turn switch-latency logging on
    On,

    /// Turn switch-latency logging off
    Off,

    /// Print whether switch-latency logging is currently on
    Status,

    /// Tail perf.log
    Logs {
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
enum ComponentCommand {
    /// Tail this component's log file (~/.local/state/lanes/<name>.log) -
    /// sugar over `tail`/`tail -f` on the right path, same as `docker logs
    /// -f` wraps tailing a container's own log file.
    Logs {
        /// Follow the file for new lines, like `tail -f`
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
enum ContextCommand {
    /// Set a context's display name in every file it's shown in today:
    /// lane.toml's `name`, zellij-tabs.toml's `name` for its terminal
    /// session, and its board.yaml `title` - whichever of those apply.
    SetName { id: String, name: String },

    /// Disable a context's lane facet - its lane.toml is kept on disk but
    /// behaves as if the file didn't exist (no lane, invisible to cycling
    /// and the dashboard) until re-enabled. Distinct from `lanes
    /// deactivate`, which drops a still-visible lane out of the cycling
    /// set without hiding it.
    DisableLane { id: String },

    /// Re-enable a context's lane facet, restoring its lane.toml.
    EnableLane { id: String },

    /// Disable a context's tab facet - its tab.toml's `scope` is kept on
    /// disk but behaves as if the file didn't exist (no zellij session,
    /// no repo anchor) until re-enabled.
    DisableTab { id: String },

    /// Re-enable a context's tab facet, restoring its tab.toml's `scope`.
    EnableTab { id: String },

    /// Disable a context's board facet - its board.toml is kept on disk
    /// but behaves as if the file didn't exist (no Boards project, no
    /// placement folded into the lane's targets) until re-enabled.
    DisableBoard { id: String },

    /// Re-enable a context's board facet, restoring its board.toml.
    EnableBoard { id: String },
}

#[derive(Subcommand)]
enum SessionsCommand {
    /// Switch to a specific Claude session (raise its WezTerm tab and Zellij pane)
    Switch { id: String },

    /// Switch to the next Claude session, cycling
    Next {
        /// Also request that Lanes Switch show+focus its window, from this
        /// same CLI invocation rather than a separate one - two concurrent
        /// processes each doing their own unguarded load/save of state.kdl
        /// could silently clobber each other's write.
        #[arg(long)]
        show: bool,
    },

    /// Switch to the previous Claude session, cycling
    Prev {
        #[arg(long)]
        show: bool,
    },
}

#[derive(Subcommand)]
enum LaneCycleCommand {
    /// Focus the next active, reachable lane, cycling
    Next {
        #[arg(long)]
        show: bool,
    },

    /// Focus the previous active, reachable lane, cycling
    Prev {
        #[arg(long)]
        show: bool,
    },
}

#[derive(Subcommand)]
enum CycleCommand {
    /// Cycle forward using the configured policy
    Next {
        #[arg(long)]
        show: bool,
    },

    /// Cycle backward using the configured policy
    Prev {
        #[arg(long)]
        show: bool,
    },
}

#[derive(Subcommand)]
enum TabsCommand {
    /// List cached session -> WezTerm tab ID mappings
    List,

    /// Manually associate a Zellij session with a WezTerm tab ID
    ///
    /// Tab titles aren't guaranteed to relate to the session name (e.g. tools
    /// like `infra lanes up` set arbitrary human-readable titles), so
    /// automatic lookup can't always find the right tab on its own. Find the
    /// tab ID with `wezterm cli list --format json` and set it here once;
    /// activation will use the cached ID from then on.
    Set { session: String, id: u64 },

    /// Remove a session's cached WezTerm tab ID entirely
    ///
    /// Meant to be called by whatever tool actually closes a session's
    /// WezTerm tab (e.g. `ztabs deactivate`) at the moment it does
    /// so, so the cache never sits there pointing at a tab that's already
    /// gone.
    Clear { session: String },
}

/// Shared body for the six `ctx enable-*`/`disable-*` commands: `facet` is
/// "lane", "tab", or "board", picking which facet file's `enabled` key to
/// flip (see `config::set_lane_enabled`/`set_tab_enabled`/
/// `set_board_enabled`).
fn set_facet_enabled_or_exit(id: &str, facet: &str, enabled: bool) {
    let result = match facet {
        "lane" => lanes::config::set_lane_enabled(id, enabled),
        "tab" => lanes::config::set_tab_enabled(id, enabled),
        "board" => lanes::config::set_board_enabled(id, enabled),
        _ => unreachable!("set_facet_enabled_or_exit called with an unknown facet {facet:?}"),
    };
    if let Err(e) = result {
        eprintln!("error: {}", e);
        std::process::exit(1);
    }
    println!("{}d {}'s {} facet", if enabled { "enable" } else { "disable" }, id, facet);
}

/// `snapshot`/`signals` used to build this from `gather_lanes()`, a second,
/// independently-drifting implementation of "turn facts into lane
/// signals" alongside `lensd::snapshot_from_mirror` (the one `switch-ui`
/// uses). Collapsed to one: this now requires a live lensd connection,
/// where it used to work standalone - a real behavior change, not just a
/// refactor.
fn lanes_snapshot_from_lensd() -> lanes::model::LanewiseSnapshot {
    let cfg = lanes::config::Config::load();
    let path = lanes::lensd::default_socket();
    let mirror = match lanes::lensd::read_snapshot(&path, std::time::Duration::from_secs(5)) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: cannot read lensd socket {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    lanes::lensd::snapshot_from_mirror(&cfg, &mirror)
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Focus { id } => {
            let cfg = lanes::config::Config::load();
            cmd::focus::run(id, &cfg);
        }

        Command::Activate { id } => {
            let cfg = lanes::config::Config::load();
            cmd::activate::run(id, &cfg);
        }

        Command::Deactivate { id } => {
            let cfg = lanes::config::Config::load();
            cmd::deactivate::run(id, &cfg);
        }

        Command::Current { name } => {
            match lanes::state::read_focused_lane() {
                Some(id) if name => {
                    let cfg = lanes::config::Config::load();
                    match cfg.lanes.iter().find(|l| l.id == id) {
                        Some(lane) => println!("{}", lane.name),
                        None => {
                            eprintln!("error: focused lane '{}' not found in config", id);
                            std::process::exit(1);
                        }
                    }
                }
                Some(id) => println!("{}", id),
                None => {
                    eprintln!("no focused lane");
                    std::process::exit(1);
                }
            }
        }

        Command::ActivateDriver { driver } => {
            if let Err(e) = lanes::activate_context(&driver) {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }

        Command::Init => lanes::logging::init_state(),

        Command::Doctor { fix } => cmd::doctor::run(fix),

        Command::Ls { long, json } => {
            let cfg = lanes::config::Config::load();
            cmd::ls::run(&cfg.contexts, long, json);
        }

        Command::Snapshot { out } => {
            let snapshot = lanes_snapshot_from_lensd();
            let json = serde_json::to_string_pretty(&snapshot).expect("serialization failed");
            match out {
                Some(path) => std::fs::write(&path, &json).expect("failed to write output file"),
                None => println!("{}", json),
            }
        }

        Command::Signals => {
            let snapshot = lanes_snapshot_from_lensd();
            let signaled: Vec<_> = snapshot.lanes.iter()
                .filter(|l| l.has_signals())
                .collect();
            println!("{}", serde_json::to_string_pretty(&signaled).unwrap());
        }

        Command::Context { command } => match command {
            ContextCommand::SetName { id, name } => {
                let cfg = lanes::config::Config::load();
                match lanes::context::set_display_name(&cfg, &id, &name) {
                    Ok(touched) => println!("updated {}: {}", id, touched.join(", ")),
                    Err(e) => {
                        eprintln!("error: {}", e);
                        std::process::exit(1);
                    }
                }
            }

            ContextCommand::DisableLane { id } => set_facet_enabled_or_exit(&id, "lane", false),
            ContextCommand::EnableLane { id } => set_facet_enabled_or_exit(&id, "lane", true),
            ContextCommand::DisableTab { id } => set_facet_enabled_or_exit(&id, "tab", false),
            ContextCommand::EnableTab { id } => set_facet_enabled_or_exit(&id, "tab", true),
            ContextCommand::DisableBoard { id } => set_facet_enabled_or_exit(&id, "board", false),
            ContextCommand::EnableBoard { id } => set_facet_enabled_or_exit(&id, "board", true),
        },

        Command::Sessions { command } => match command {
            SessionsCommand::Switch { id } => {
                if let Err(e) = lanes::switch_claude_session(&id) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }

            SessionsCommand::Next { show } => {
                // Show first, switch second: the switch ends with WezTerm
                // itself grabbing focus (activate_wezterm_tab), which should
                // win. Showing afterward would call set_focus() on Lanes
                // Switch's own window *after* that, stealing focus right
                // back - and delay the window appearing until after the
                // switch, possibly past when Control-Option was released.
                if show {
                    lanes::notify_switch_show();
                }
                if let Err(e) = lanes::cycle_claude_session(1) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }

            SessionsCommand::Prev { show } => {
                if show {
                    lanes::notify_switch_show();
                }
                if let Err(e) = lanes::cycle_claude_session(-1) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
        },

        Command::LaneCycle { command } => match command {
            LaneCycleCommand::Next { show } => {
                if show {
                    lanes::notify_switch_show();
                }
                if let Err(e) = lanes::cycle_lane(1) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }

            LaneCycleCommand::Prev { show } => {
                if show {
                    lanes::notify_switch_show();
                }
                if let Err(e) = lanes::cycle_lane(-1) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
        },

        Command::Cycle { command } => match command {
            CycleCommand::Next { show } => {
                if show {
                    lanes::notify_switch_show();
                }
                if let Err(e) = lanes::cycle(1) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }

            CycleCommand::Prev { show } => {
                if show {
                    lanes::notify_switch_show();
                }
                if let Err(e) = lanes::cycle(-1) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }
        },

        Command::Tabs { command } => match command {
            TabsCommand::List => {
                for (session, id) in lanes::state::all_wezterm_tab_ids() {
                    println!("{}  tab_id={}", session, id);
                }
            }

            TabsCommand::Set { session, id } => {
                lanes::state::set_wezterm_tab_id(&session, id);
                println!("{}  tab_id={}", session, id);
            }

            TabsCommand::Clear { session } => {
                lanes::state::clear_wezterm_tab_id(&session);
                println!("{}  cleared", session);
            }
        },

        Command::IsPinned => {
            let pinned = lanes::state::read_switch_pinned();
            println!("{}", pinned);
            if !pinned {
                std::process::exit(1);
            }
        }

        Command::TogglePinned => {
            let pinned = !lanes::state::read_switch_pinned();
            lanes::state::set_switch_pinned(pinned);
            println!("{}", pinned);
            if !pinned {
                std::process::exit(1);
            }
        }

        Command::ToggleEditMode => {
            let enabled = !lanes::state::read_edit_mode();
            lanes::logging::perf("edit_mode.cli_toggle", &format!("enabled={enabled}"));
            lanes::state::set_edit_mode(enabled);
            lanes::notify_switch_edit_mode(enabled);
            println!("{}", enabled);
        }

        Command::ToggleShowInactive => {
            // Blocked outright while edit mode is on, not just visually
            // deferred - edit mode already shows every lane regardless of
            // this preference (see App.svelte's visibleLanes), so there's
            // nothing for it to affect right now. The value must not
            // change at all here, not change-but-not-render: writing it
            // anyway and only skipping the redraw meant the preference had
            // already silently flipped by the time edit mode was left.
            // Pings the running app to acknowledge the keypress (a brief
            // border pulse - see App.svelte) rather than surfacing this as
            // a Hammerspoon error alert: it's not a failure, it's a
            // real no-op, and a system alert is a much louder signal than
            // this deserves.
            if lanes::state::read_edit_mode() {
                lanes::notify_switch_show_inactive_noop();
                println!("no-op: edit mode is on");
            } else {
                let show = !lanes::state::read_show_inactive();
                lanes::state::set_show_inactive(show);
                println!("{}", show);
            }
        }

        Command::ShellInit { shell } => {
            let name = match shell {
                ShellKind::Fish => "fish",
            };
            cmd::shell_init::run(name);
        }

        Command::ShowSwitch => {
            lanes::notify_switch_show();
        }

        Command::HideSwitch => {
            lanes::notify_switch_hide();
        }

        Command::SwitchUi { command: ComponentCommand::Logs { follow } } => {
            cmd::logs::run("switch-ui.log", follow);
        }

        Command::Hammerspoon { command: ComponentCommand::Logs { follow } } => {
            cmd::logs::run("hammerspoon.log", follow);
        }

        Command::Diagnostics { command } => match command {
            DiagnosticsCommand::On => {
                lanes::state::set_diagnostics_enabled(true);
                println!("true");
            }
            DiagnosticsCommand::Off => {
                lanes::state::set_diagnostics_enabled(false);
                println!("false");
            }
            DiagnosticsCommand::Status => {
                println!("{}", lanes::state::read_diagnostics_enabled());
            }
            DiagnosticsCommand::Logs { follow } => {
                cmd::logs::run("perf.log", follow);
            }
        },
    }
}
