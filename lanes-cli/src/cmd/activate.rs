use std::process::Command;

use lanes::config::Config;
use lanes::model::{Facet, Lane};
use lanes::zone;

pub fn run(lane_id: &str, cfg: &Config) {
    let lane = cfg.lanes.iter().find(|l| l.id == lane_id);
    let lane = match lane {
        Some(l) => l,
        None => {
            eprintln!("error: lane not found: {}", lane_id);
            std::process::exit(1);
        }
    };

    for facet in &lane.facets {
        match facet {
            Facet::Terminal { session } => activate_terminal(session, lane.display_name()),
            Facet::Window { path, zone } => activate_window(path, zone, cfg, lane),
        }
    }
}

fn wezterm_socket() -> Option<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/Users/bmiller".to_string());
    let dir = std::path::PathBuf::from(home).join(".local/share/wezterm");
    let mut socks: Vec<_> = std::fs::read_dir(&dir).ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("gui-sock-"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    socks.sort_by(|a, b| b.0.cmp(&a.0));
    socks.into_iter().next().map(|(_, p)| p.to_string_lossy().into_owned())
}

fn activate_terminal(session: &str, display_name: &str) {
    let sock = wezterm_socket();

    // Find the WezTerm tab whose title matches the lane display name or session name.
    let mut cmd = Command::new("/opt/homebrew/bin/wezterm");
    cmd.args(["cli", "list", "--format", "json"]);
    if let Some(ref s) = sock {
        cmd.env("WEZTERM_UNIX_SOCKET", s);
    }
    let output = cmd.output();

    let output = match output {
        Ok(o) => o,
        Err(e) => { eprintln!("wezterm cli list failed: {}", e); return; }
    };

    let json: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(v) => v,
        Err(_) => { eprintln!("could not parse wezterm cli list output"); return; }
    };

    let tab_id = json.as_array()
        .and_then(|panes| {
            panes.iter().find(|p| {
                p["tab_title"].as_str().map_or(false, |t| t == display_name || t == session)
            })
        })
        .and_then(|p| p["tab_id"].as_u64());

    match tab_id {
        None => eprintln!("no WezTerm tab found for lane '{}' (session '{}')", display_name, session),
        Some(id) => {
            Command::new("open").args(["-a", "WezTerm"]).output().ok();
            let mut cmd = Command::new("/opt/homebrew/bin/wezterm");
            cmd.args(["cli", "activate-tab", "--tab-id", &id.to_string()]);
            if let Some(ref s) = sock {
                cmd.env("WEZTERM_UNIX_SOCKET", s);
            }
            if let Err(e) = cmd.output() {
                eprintln!("wezterm cli activate-tab failed: {}", e);
            }
        }
    }
}

fn activate_window(path: &str, zone: &str, cfg: &Config, _lane: &Lane) {
    let bundle_id = match parse_bundle_id(path) {
        Some(id) => id,
        None => { eprintln!("warning: could not parse bundle id from path '{}'", path); return; }
    };

    let rect = match lanes::zone::parse(zone) {
        Ok(r) => r,
        Err(e) => { eprintln!("warning: {}", e); return; }
    };

    let uuid = match cfg.monitor_uuid(&rect.monitor_handle) {
        Some(u) => u.to_string(),
        None => { eprintln!("warning: monitor handle '{}' not found in config", rect.monitor_handle); return; }
    };

    let lua = format!(
        "local s=nil; \
         for _,sc in ipairs(hs.screen.allScreens()) do \
           if sc:getUUID()=='{uuid}' then s=sc; break end \
         end; \
         if s then \
           local apps=hs.application.applicationsForBundleID('{bundle}'); \
           local a=apps and apps[1]; \
           if a then \
             local w=a:mainWindow(); \
             if w then \
               local f=s:frame(); \
               w:setFrame({{x=f.x+{x}*f.w, y=f.y+{y}*f.h, w={ww}*f.w, h={h}*f.h}}) \
             end \
           end \
         end",
        uuid = uuid,
        bundle = bundle_id,
        x = rect.x,
        y = rect.y,
        ww = rect.w,
        h = rect.h,
    );

    match Command::new("hs").args(["-c", &lua]).output() {
        Err(e) => eprintln!("warning: hs call failed for '{}': {}", bundle_id, e),
        Ok(o) if !o.status.success() => {
            eprintln!("warning: hs returned error for '{}':\n{}", bundle_id, String::from_utf8_lossy(&o.stderr));
        }
        _ => {}
    }
}

/// Extract bundle ID from a path segment like `app:com.github.wez.wezterm / window`.
fn parse_bundle_id(path: &str) -> Option<String> {
    let first = path.split(" / ").next()?;
    let bundle = first.strip_prefix("app:")?;
    Some(bundle.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundle_id() {
        assert_eq!(
            parse_bundle_id("app:com.github.wez.wezterm / window"),
            Some("com.github.wez.wezterm".to_string())
        );
    }

    #[test]
    fn parses_bundle_id_bare() {
        assert_eq!(
            parse_bundle_id("app:org.mozilla.firefox / window"),
            Some("org.mozilla.firefox".to_string())
        );
    }
}
