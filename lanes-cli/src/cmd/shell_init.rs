//! `lanes shell-init <shell>` - prints a shell hook that records
//! long-running foreground commands for the `shell` driver. The hook is the
//! shell-specific half of that driver (see drivers/shell.rs); it normalises
//! to the one record schema the driver reads.

/// `done`/`failed` via fish's `$CMD_DURATION`, no timer, no background
/// process. `running` is written here too, unconditionally on every
/// command (not just ones that turn out long) - the hook can't know in
/// advance how long a command will take, so it can't gate the write the
/// way `done`/`failed` gate on `LANES_SHELL_LATCH_SECS`. The "don't flash
/// a chip for `ls`" floor (`LANES_SHELL_NOTICE_SECS`) is enforced
/// Lanes-side instead, on read - see `command_signal` in `lib.rs`.
const FISH_HOOK: &str = r####"# lanes shell-init fish - long-running-command signal
# Source from ~/.config/fish/config.fish:  lanes shell-init fish | source

set -q LANES_SHELL_LATCH_SECS; or set -g LANES_SHELL_LATCH_SECS 30
set -g __lanes_shell_dir "$HOME/.local/state/lanes/shell"

# Path for this pane's record, or empty if we can't identify the pane
# (not inside zellij, or $ZELLIJ_PANE_ID unset) - in which case the hook
# does nothing.
function __lanes_shell_path
    test -n "$ZELLIJ_SESSION_NAME" -a -n "$ZELLIJ_PANE_ID"; or return 1
    echo "$__lanes_shell_dir/$ZELLIJ_SESSION_NAME--$ZELLIJ_PANE_ID.json"
end

function __lanes_shell_preexec --on-event fish_preexec
    set -l p (__lanes_shell_path); or return
    set -g __lanes_shell_cmd $argv[1]
    set -g __lanes_shell_started (date -u +%Y-%m-%dT%H:%M:%SZ)

    mkdir -p "$__lanes_shell_dir"
    set -l cmd (string replace -a '\\' '\\\\' -- "$__lanes_shell_cmd" | string replace -a '"' '\\"')
    set -l argv0 (string split -- ' ' "$__lanes_shell_cmd")[1]
    printf '{"v":1,"shell":"fish","session":"%s","pane":%s,"argv0":"%s","cmd":"%s","started_at":"%s","state":"running","ended_at":null,"exit_code":null}\n' \
        "$ZELLIJ_SESSION_NAME" "$ZELLIJ_PANE_ID" "$argv0" "$cmd" "$__lanes_shell_started" >"$p"
end

function __lanes_shell_postexec --on-event fish_postexec
    set -l code $status
    set -l p (__lanes_shell_path); or return
    set -l ms "$CMD_DURATION"

    # No duration, signal-terminated (Ctrl-C a dev server - stopped on
    # purpose), or under the latch threshold - clear the running record
    # preexec wrote rather than leaving a stale "running" chip with nothing
    # left to ever update or remove it.
    if test -z "$ms"; or test $code -gt 128; or test "$ms" -lt (math "$LANES_SHELL_LATCH_SECS * 1000")
        rm -f "$p"
        return
    end

    mkdir -p "$__lanes_shell_dir"
    set -l state done
    test $code -ne 0; and set state failed

    set -l cmd (string replace -a '\\' '\\\\' -- "$__lanes_shell_cmd" | string replace -a '"' '\\"')
    set -l argv0 (string split -- ' ' "$__lanes_shell_cmd")[1]
    set -l now (date -u +%Y-%m-%dT%H:%M:%SZ)

    printf '{"v":1,"shell":"fish","session":"%s","pane":%s,"argv0":"%s","cmd":"%s","started_at":"%s","state":"%s","ended_at":"%s","exit_code":%s}\n' \
        "$ZELLIJ_SESSION_NAME" "$ZELLIJ_PANE_ID" "$argv0" "$cmd" \
        "$__lanes_shell_started" "$state" "$now" "$code" >"$p"
end
"####;

pub fn run(shell: &str) {
    match shell {
        "fish" => print!("{FISH_HOOK}"),
        other => {
            eprintln!("lanes shell-init: unsupported shell {other:?} (only 'fish' for now)");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FISH_HOOK;

    #[test]
    fn fish_hook_wires_both_events_and_the_core_guards() {
        // A cheap smoke test against accidental edits to the template - the
        // behaviour itself is covered by a shell-level check, but these are
        // the load-bearing lines.
        assert!(FISH_HOOK.contains("--on-event fish_preexec"));
        assert!(FISH_HOOK.contains("--on-event fish_postexec"));
        // threshold gate (below it: no record written)
        assert!(FISH_HOOK.contains("LANES_SHELL_LATCH_SECS"));
        assert!(FISH_HOOK.contains(r#"test "$ms" -lt"#));
        // signal-killed commands clear rather than latch
        assert!(FISH_HOOK.contains("test $code -gt 128"));
        // record carries the schema version the driver expects
        assert!(FISH_HOOK.contains(r#""v":1"#));
        assert!(FISH_HOOK.contains(r#""state":"%s""#));
        // preexec writes a running record unconditionally, every command -
        // the "don't flash for `ls`" floor is enforced Lanes-side on read,
        // not by withholding the write (the hook can't know in advance how
        // long a command will take).
        assert!(FISH_HOOK.contains(r#""state":"running""#));
        // postexec clears (not just skips) the running record on every
        // early-return path, or a fast/killed command would leave a stale
        // "running" chip with nothing left to update or remove it.
        let postexec = FISH_HOOK.split("function __lanes_shell_postexec").nth(1).unwrap();
        let early_return = postexec.split("mkdir -p").next().unwrap();
        assert!(early_return.contains(r#"rm -f "$p""#));
    }

    #[test]
    fn fish_hook_only_acts_inside_a_zellij_pane() {
        // No $ZELLIJ_PANE_ID -> __lanes_shell_path returns 1 -> hook is a
        // no-op. Guards against a record path that would collide across
        // panes or land outside the state dir.
        assert!(FISH_HOOK.contains(r#"test -n "$ZELLIJ_SESSION_NAME" -a -n "$ZELLIJ_PANE_ID""#));
    }
}
