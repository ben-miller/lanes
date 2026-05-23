use std::path::PathBuf;
use std::process::Command;

struct Check {
    label: &'static str,
    status: Status,
    message: String,
    hint: Option<String>,
}

enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn symbol(&self) -> &str {
        match self {
            Status::Ok => "✓",
            Status::Warn => "⚠",
            Status::Fail => "✗",
        }
    }
}

pub fn run() {
    let cfg = lanes::config::Config::load();

    let mut checks = vec![check_lanes_registry()];

    if cfg.driver_enabled("zellij") {
        checks.push(check_zellij());
    }
    if cfg.driver_enabled("claude") {
        checks.push(check_claude());
    }
    if cfg.driver_enabled("brotab") {
        checks.push(check_brotab());
    };

    let mut any_fail = false;
    for c in &checks {
        println!("{} {}: {}", c.status.symbol(), c.label, c.message);
        if let Some(hint) = &c.hint {
            println!("  {}", hint);
        }
        if matches!(c.status, Status::Fail) {
            any_fail = true;
        }
    }

    if any_fail {
        std::process::exit(1);
    }
}

fn check_zellij() -> Check {
    let version_out = Command::new("zellij").arg("--version").output();
    match version_out {
        Err(_) => Check {
            label: "zellij",
            status: Status::Fail,
            message: "not found".to_string(),
            hint: Some("brew install zellij".to_string()),
        },
        Ok(out) => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let sessions_out = Command::new("zellij")
                .args(["list-sessions", "--no-formatting", "--short"])
                .output();
            let session_summary = match sessions_out {
                Ok(o) => {
                    let count = String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                        .count();
                    format!("{} session(s)", count)
                }
                Err(_) => "could not list sessions".to_string(),
            };
            Check {
                label: "zellij",
                status: Status::Ok,
                message: format!("{} — {}", version, session_summary),
                hint: None,
            }
        }
    }
}

fn check_claude() -> Check {
    let registry = PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".claude")
        .join("active-sessions");

    match std::fs::read_dir(&registry) {
        Err(_) => Check {
            label: "claude sessions",
            status: Status::Warn,
            message: format!("registry not found at {}", registry.display()),
            hint: Some("start a Claude Code session to create the registry".to_string()),
        },
        Ok(entries) => {
            let count = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |x| x == "json"))
                .count();
            Check {
                label: "claude sessions",
                status: Status::Ok,
                message: format!("{} active session(s)", count),
                hint: None,
            }
        }
    }
}

fn check_brotab() -> Check {
    let bt = Command::new("bt").arg("clients").output();
    match bt {
        Err(_) => Check {
            label: "brotab",
            status: Status::Fail,
            message: "bt not found — browser facet unavailable".to_string(),
            hint: Some(
                "pipx install brotab  →  bt install  →  install Firefox extension from addons.mozilla.org/en-US/firefox/addon/brotab/".to_string(),
            ),
        },
        Ok(out) if out.stdout.is_empty() => Check {
            label: "brotab",
            status: Status::Warn,
            message: "bt found but no connected browsers".to_string(),
            hint: Some(
                "ensure the BroTab extension is installed in Firefox and bt install has been run".to_string(),
            ),
        },
        Ok(out) => {
            let clients = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| !l.trim().is_empty())
                .count();
            Check {
                label: "brotab",
                status: Status::Ok,
                message: format!("{} connected browser(s)", clients),
                hint: None,
            }
        }
    }
}

fn check_lanes_registry() -> Check {
    let path = PathBuf::from(std::env::var("HOME").unwrap_or_default())
        .join(".config")
        .join("lanes")
        .join("registry.toml");

    match std::fs::read_to_string(&path) {
        Err(_) => Check {
            label: "lanes registry",
            status: Status::Warn,
            message: format!("not found at {}", path.display()),
            hint: Some("create ~/.config/lanes/registry.toml to define lanes".to_string()),
        },
        Ok(content) => {
            let count = content.lines().filter(|l| l.trim() == "[[lanes]]").count();
            Check {
                label: "lanes registry",
                status: Status::Ok,
                message: format!("{} lane(s) defined", count),
                hint: None,
            }
        }
    }
}
