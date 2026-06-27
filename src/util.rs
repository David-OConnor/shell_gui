//! Misc utility functionality.

use std::{fs, io, path::Path};

use shell::{commands, path_from_args, quiet_command};

use crate::{OutType, out_type_for, state::State, util};

/// Read a file into a `String`. Used by the `cat` builtin to populate the
/// output pane.
pub fn read_file(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}

/// Note: In `shell`, this is in the `commands` module. To prevent possible confusion, we don't
/// have an additoinal `commands` module in this program.
pub fn handle_cmd(state: &mut State, cmd: &str, input: &str, args: &str) {
    // While the active tab is connected, route commands to the remote. Only
    // the session-management keywords are intercepted; everything else (exec
    // mode) runs on the remote shell. (PTY-mode input is sent directly from the
    // input row, not here — see `gui::term_in`.)
    if state.active_remote().is_some() {
        match cmd {
            "exit" | "quit" | "logout" | "disconnect" => state.disconnect_remote(),
            "mode" => state.toggle_remote_mode(),
            _ => state.run_remote_exec(input),
        }
        return;
    }

    match cmd {
        "exit" | "quit" => {
            state.push_out("(use the window close button to exit)", OutType::StdErr);
        }

        // `ssh <index>` connects the active tab to a saved remote (same as the
        // per-remote buttons in the tab strip); `ssh [user@]host[:port]`
        // connects to a freeform target using a keyring-stored password.
        "ssh" => state.cmd_ssh(args),

        "sync" => {
            // Delegate to the shared implementation. Clone `cwd` so the
            // sink closure can borrow `state` mutably for `push_out` without
            // overlapping with a `&state.cwd` borrow.
            let cwd = state.cwd().to_path_buf();
            commands::sync(args, &cwd, &mut |kind, msg| {
                state.push_out(msg, out_type_for(kind));
            });
        }

        "logs" => {
            // GUI uses `follow = false`: the live `-f` tail would block the
            // UI thread indefinitely, so the shared impl falls back to a
            // bounded `-n 200` snapshot in this mode. On non-Linux the
            // shared impl emits a friendly "only supported on Linux" error.
            commands::logs(args, false, &mut |kind, msg| {
                state.push_out(msg, out_type_for(kind));
            });
        }

        "cat" => {
            let target = path_from_args(
                state.home.as_deref(),
                state.cwd(),
                &state.dir_bookmarks,
                args,
            );
            match util::read_file(&target) {
                Ok(text) => state.push_out(text, OutType::StdOut),
                Err(e) => {
                    state.push_out(format!("cat: {}: {e}", target.display()), OutType::StdErr)
                }
            }
        }

        "del" => {
            let (sub, rest) = match args.find(char::is_whitespace) {
                Some(i) => (&args[..i], args[i..].trim()),
                None => (args, ""),
            };
            match sub {
                "bm" => match rest.parse::<usize>() {
                    Ok(idx) => {
                        if idx < state.dir_bookmarks.len() {
                            let path = state.dir_bookmarks.remove(idx);
                            state.refresh_bookmarks_filter();
                            state.push_out(
                                format!("Deleted bookmark: {}", path.display()),
                                OutType::StdOut,
                            );
                            if let Err(e) = state.save() {
                                state.push_out(
                                    format!("del bm: failed to save bookmarks: {e}"),
                                    OutType::StdErr,
                                );
                            }
                        } else {
                            let len = state.dir_bookmarks.len();
                            state.push_out(
                                format!("del bm: no bookmark at index {idx} (have {len})"),
                                OutType::StdErr,
                            );
                        }
                    }
                    Err(_) => state.push_out(
                        "del bm: expected a number, e.g. `del bm 4`",
                        OutType::StdErr,
                    ),
                },
                "" => state.push_out("del: usage: del bm <number>", OutType::StdErr),
                other => state.push_out(
                    format!("del: unknown target `{other}` (expected `bm`)"),
                    OutType::StdErr,
                ),
            }
        }

        "cd" => {
            // Remember the recent-dir index if the arg parsed as a number
            // so we can prune the entry on a `NotFound` cd failure (the
            // path was deleted/moved on disk).
            let (target, recent_idx) = if let Ok(idx) = args.parse::<usize>() {
                match state.recent_dirs.get(idx).map(|r| r.path.clone()) {
                    Some(p) => (Some(p), Some(idx)),
                    None => {
                        state.push_out(
                            format!("cd: no recent directory at index {idx}"),
                            OutType::StdErr,
                        );
                        (None, None)
                    }
                }
            } else {
                (
                    Some(path_from_args(
                        state.home.as_deref(),
                        state.cwd(),
                        &state.dir_bookmarks,
                        args,
                    )),
                    None,
                )
            };

            if let Some(target) = target {
                let before = state.cwd().to_path_buf();
                state.change_dir(target.clone());
                let failed = state.cwd() == before;
                if failed {
                    if let Some(i) = recent_idx {
                        if let Err(e) = std::fs::metadata(&target) {
                            if e.kind() == io::ErrorKind::NotFound
                                && state
                                    .recent_dirs
                                    .get(i)
                                    .map(|r| r.path == target)
                                    .unwrap_or(false)
                            {
                                state.recent_dirs.remove(i);
                                state.refresh_recent_dirs_filter();
                                state.push_out(
                                    format!("cd: removed stale recent-dir entry {i}"),
                                    OutType::StdErr,
                                );
                                let _ = state.save();
                            }
                        }
                    }
                }
            }
        }

        "bm" => match args.parse::<usize>() {
            Ok(idx) => match state.dir_bookmarks.get(idx).cloned() {
                Some(target) => state.change_dir(target),
                None => state.push_out(format!("bm: no bookmark at index {idx}"), OutType::StdErr),
            },
            Err(_) => state.push_out("bm: usage: bm <number>", OutType::StdErr),
        },

        _ => {
            let result = if cfg!(windows) {
                quiet_command("pwsh")
                    .args(["-NoProfile", "-NoLogo", "-Command", input])
                    .current_dir(state.cwd())
                    .output()
            } else {
                quiet_command("sh")
                    .args(["-c", input])
                    .current_dir(state.cwd())
                    .output()
            };
            match result {
                Ok(out) => {
                    if !out.stdout.is_empty() {
                        state.push_out(
                            String::from_utf8_lossy(&out.stdout).into_owned(),
                            OutType::StdOut,
                        );
                    }
                    if !out.stderr.is_empty() {
                        state.push_out(
                            String::from_utf8_lossy(&out.stderr).into_owned(),
                            OutType::StdErr,
                        );
                    }
                }
                Err(e) => state.push_out(format!("shell: {e}"), OutType::StdErr),
            }
        }
    }
}
