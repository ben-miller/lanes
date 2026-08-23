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
    Doctor,

    /// List configured lanes and their facets
    List {
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

    /// Manage Claude sessions
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
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

    /// Ask the running Lanes Switch app to show+focus its window, without
    /// changing pin state. See `IsPinned` for the state.kdl watch mechanism.
    ShowSwitch,

    /// Ask the running Lanes Switch app to hide its window, without
    /// changing pin state. See `IsPinned` for the state.kdl watch mechanism.
    HideSwitch,

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

        Command::Init => lanes::logging::init_state(),

        Command::Doctor => cmd::doctor::run(),

        Command::List { json } => {
            let cfg = lanes::config::Config::load();
            cmd::list::run(&cfg.lanes, json);
        }

        Command::Snapshot { out } => {
            let cfg = lanes::config::Config::load();
            let snapshot = lanes::gather_lanes(&cfg);
            let json = serde_json::to_string_pretty(&snapshot).expect("serialization failed");
            match out {
                Some(path) => std::fs::write(&path, &json).expect("failed to write output file"),
                None => println!("{}", json),
            }
        }

        Command::Signals => {
            let cfg = lanes::config::Config::load();
            let snapshot = lanes::gather_lanes(&cfg);
            let signaled: Vec<_> = snapshot.lanes.iter()
                .filter(|l| l.has_signals())
                .collect();
            println!("{}", serde_json::to_string_pretty(&signaled).unwrap());
        }

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
    }
}
