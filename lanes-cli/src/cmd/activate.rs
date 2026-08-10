use lanes::config::Config;

pub fn run(lane_id: &str, _cfg: &Config) {
    lanes::activate_lane(lane_id, true);
}
