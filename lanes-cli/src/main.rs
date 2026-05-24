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

    /// Manage Claude sessions
    Sessions {
        #[command(subcommand)]
        command: SessionsCommand,
    },
}

#[derive(Subcommand)]
enum SessionsCommand {
    /// List all active Claude sessions
    List,

    /// Get a single session by ID
    Get { id: String },
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Command::Activate { id } => {
            let cfg = lanes::config::Config::load();
            cmd::activate::run(&id, &cfg);
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
        },
    }
}
