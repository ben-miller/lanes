use std::process::Command;

/// Shells out to the real `tail` rather than re-implementing file-following -
/// it already handles growing files, rotation, and Ctrl+C correctly, and
/// inherits this process's stdio so output streams straight to the
/// terminal exactly like running `tail` directly would.
pub fn run(file_name: &str, follow: bool) {
    let path = lanes::logging::state_dir().join(file_name);
    if !path.exists() {
        eprintln!("no log yet at {}", path.display());
        std::process::exit(1);
    }

    let mut cmd = Command::new("tail");
    if follow {
        cmd.args(["-n", "+1", "-f"]);
    }
    cmd.arg(&path);

    match cmd.status() {
        Ok(status) if !status.success() => std::process::exit(status.code().unwrap_or(1)),
        Ok(_) => {}
        Err(e) => {
            eprintln!("error: failed to run tail: {}", e);
            std::process::exit(1);
        }
    }
}
