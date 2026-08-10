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
    /// Activate a lane: focus terminal and apply window placement
    Activate {
        /// Lane ID to activate
        id: String,
    },

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

    /// Print the currently active lane ID
    Current,

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
}

#[derive(Subcommand)]
enum SessionsCommand {
    /// List all active Claude sessions
    List,

    /// Get a single session by ID
    Get { id: String },

    /// Switch to a specific Claude session (raise its WezTerm tab and Zellij pane)
    Switch { id: String },

    /// Switch to the next Claude session, cycling
    Next,

    /// Switch to the previous Claude session, cycling
    Prev,
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
        Command::Activate { id } => {
            let cfg = lanes::config::Config::load();
            cmd::activate::run(&id, &cfg);
        }

        Command::Current => {
            match lanes::state::read_current_lane() {
                Some(id) => println!("{}", id),
                None => {
                    eprintln!("no active lane");
                    std::process::exit(1);
                }
            }
        }

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
            SessionsCommand::List => {
                let snapshot = lanes::gather();
                let sessions: Vec<_> = snapshot
                    .resources
                    .iter()
                    .filter(|r| {
                        matches!(
                            &r.selector,
                            lanes::model::Selector::Terminal(sel) if sel.driver == "claude"
                        )
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&sessions).unwrap());
            }

            SessionsCommand::Get { id } => {
                let snapshot = lanes::gather();
                let session = snapshot.resources.iter().find(|r| r.locator == id);
                match session {
                    Some(s) => println!("{}", serde_json::to_string_pretty(s).unwrap()),
                    None => {
                        eprintln!("error: session not found: {}", id);
                        std::process::exit(1);
                    }
                }
            }

            SessionsCommand::Switch { id } => {
                if let Err(e) = lanes::switch_claude_session(&id) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }

            SessionsCommand::Next => {
                if let Err(e) = lanes::cycle_claude_session(1) {
                    eprintln!("error: {}", e);
                    std::process::exit(1);
                }
            }

            SessionsCommand::Prev => {
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
    }
}
