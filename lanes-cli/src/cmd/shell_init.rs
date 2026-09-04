//! `lanes shell-init <shell>` - prints a shell hook that records
//! long-running foreground commands for the `shell` driver. The hook is the
//! shell-specific half of that driver (see drivers/shell.rs); it normalises
//! to the one record schema the driver reads.

/// Phase 1: `done` / `failed` only, via fish's `$CMD_DURATION`. No timer,
/// no background process. Phase 2 adds a `running` chip (needs a timer).
const FISH_HOOK: &str = r####"# lanes shell-init fish - long-running-command signal (Phase 1: done/failed)
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
    # Running a new command supersedes any latched done/failed chip here.
    set -l p (__lanes_shell_path); and rm -f "$p"
    set -g __lanes_shell_cmd $argv[1]
    set -g __lanes_shell_started (date -u +%Y-%m-%dT%H:%M:%SZ)
end

function __lanes_shell_postexec --on-event fish_postexec
    set -l code $status
    set -l ms "$CMD_DURATION"
    test -z "$ms"; and return
    test "$ms" -lt (math "$LANES_SHELL_LATCH_SECS * 1000"); and return

    set -l p (__lanes_shell_path); or return
    mkdir -p "$__lanes_shell_dir"

    # Signal-terminated (Ctrl-C a dev server, SIGTERM) - you stopped it on
    # purpose, so clear the record rather than latching a "failed" chip.
    if test $code -gt 128
        rm -f "$p"
        return
    end

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
    }

    #[test]
    fn fish_hook_only_acts_inside_a_zellij_pane() {
        // No $ZELLIJ_PANE_ID -> __lanes_shell_path returns 1 -> hook is a
        // no-op. Guards against a record path that would collide across
        // panes or land outside the state dir.
        assert!(FISH_HOOK.contains(r#"test -n "$ZELLIJ_SESSION_NAME" -a -n "$ZELLIJ_PANE_ID""#));
    }
}
