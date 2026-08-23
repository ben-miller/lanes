use lanes::config::Config;

pub fn run(id: Option<String>, cfg: &Config) {
    let lane_id = match lanes::resolve_lane_id(id, cfg) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    };
    lanes::focus_lane(&lane_id, true);
}
