mod ansi;
mod gui;
mod state;
mod tasks;

use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};
use shell::{
    HistoryItem, NavState, RecentDir,
    commands::{self, OutKind},
    path_from_args, save_data,
};
use state::State;

use crate::gui::{WINDOW_HEIGHT, WINDOW_WIDTH};

#[derive(Clone, Copy, PartialEq)]
pub enum OutType {
    Prompt,
    StdOut,
    StdErr,
}

/// Map the shared `commands` module's stream kind onto the GUI's output
/// row type. Used by the `sync` / `logs` sinks.
fn out_type_for(kind: OutKind) -> OutType {
    match kind {
        OutKind::Stdout => OutType::StdOut,
        OutKind::Stderr => OutType::StdErr,
    }
}

/// One line / chunk of terminal output. Lines from the same command are kept
/// as separate items so they can be coloured by stream.
pub struct OutItem {
    pub text: String,
    pub type_: OutType,
    pub dt: DateTime<Utc>,
}

/// Render a path as `~/relative` when it lives under the home directory;
/// otherwise use the absolute form. Uses forward slashes after the tilde for
/// consistency with the rest of the shell.
pub fn render_with_tilde(p: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home {
        if let Ok(rest) = p.strip_prefix(home) {
            let rest_str = rest.to_string_lossy().replace('\\', "/");
            if rest_str.is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", rest_str);
        }
    }
    p.display().to_string()
}

/// Record `cwd` in the recent-dirs list. If the path is already present we
/// remove the old entry and push a fresh one to the end so the list stays
/// deduped and the newest entry sits at the top of the display.
fn record_recent_dir(recent: &mut Vec<RecentDir>, cwd: &Path) {
    recent.retain(|r| r.path != cwd);
    recent.push(RecentDir {
        path: cwd.to_path_buf(),
        dt: Utc::now(),
    });
}

/// Runs one command line. Output is pushed into `state.ui.out` rather than
/// printed to stdout/stderr, so the GUI's terminal pane can show it.
pub fn run_command(state: &mut State, input: &str) {
    let input = input.trim();
    if input.is_empty() {
        return;
    }

    // Echo the prompt + command into the output pane so the user can see
    // what they ran (mirrors how a normal terminal scrolls). Capture the
    // recall state first so the echo preserves any `his N` / `cd N`
    // indicator, then submitting commits us back to the live input — drop
    // recall state so the next arrow starts walking from the newest entry.
    let (his_cursor, cd_cursor) = state.active_nav_cursors();
    let nav_snapshot = NavState {
        his_cursor,
        cd_cursor,
        ..NavState::new()
    };
    let prompt = state.prompt(&nav_snapshot);
    state.reset_history_nav();
    state.push_out(format!("{prompt}> {input}"), OutType::Prompt);

    let (cmd, args) = match input.find(char::is_whitespace) {
        Some(i) => (&input[..i], input[i..].trim()),
        None => (input, ""),
    };

    // `his`/`hist <n>` re-runs a previous history item.
    if cmd == "his" || cmd == "hist" {
        match args.parse::<usize>() {
            Ok(idx) => match state.history.get(idx).map(|item| item.text.clone()) {
                Some(text) => {
                    run_command(state, &text);
                    return;
                }
                None => state.push_out(
                    format!("{cmd}: no history item at index {idx}"),
                    OutType::StdErr,
                ),
            },
            Err(_) => state.push_out(format!("{cmd}: usage: {cmd} <number>"), OutType::StdErr),
        }
        return;
    }

    // `hisd <n>` re-runs a previous history item in its original working
    // directory, without changing the GUI's CWD. Bypasses the built-in
    // dispatcher and shells the command out directly, since the point is to
    // run it elsewhere on the filesystem.
    if cmd == "hisd" {
        match args.parse::<usize>() {
            Ok(idx) => match state
                .history
                .get(idx)
                .map(|item| (item.text.clone(), item.dir.clone()))
            {
                Some((text, dir)) => {
                    let result = if cfg!(windows) {
                        Command::new("pwsh")
                            .args(["-NoProfile", "-NoLogo", "-Command", &text])
                            .current_dir(&dir)
                            .output()
                    } else {
                        Command::new("sh")
                            .args(["-c", text.as_str()])
                            .current_dir(&dir)
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
                    return;
                }
                None => state.push_out(
                    format!("hisd: no history item at index {idx}"),
                    OutType::StdErr,
                ),
            },
            Err(_) => state.push_out("hisd: usage: hisd <number>", OutType::StdErr),
        }
        return;
    }

    state.history.push(HistoryItem {
        text: input.to_string(),
        dir: state.cwd().to_path_buf(),
        dt: Utc::now(),
    });
    state.refresh_history_filter();
    state.refresh_cwd_history_filter();

    // Track directories we've run real commands from (everything except `cd`).
    // Persist regardless so the new history entry hits disk.
    if cmd != "cd" {
        let cwd = state.cwd().to_path_buf();
        record_recent_dir(&mut state.recent_dirs, &cwd);
        state.refresh_recent_dirs_filter();
    }
    if let Err(e) = state.save() {
        state.push_out(
            format!("warning: failed to save state: {e}"),
            OutType::StdErr,
        );
    }

    match cmd {
        "exit" | "quit" => {
            state.push_out("(use the window close button to exit)", OutType::StdErr);
        }

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
            match tasks::read_file(&target) {
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
                Command::new("pwsh")
                    .args(["-NoProfile", "-NoLogo", "-Command", input])
                    .current_dir(state.cwd())
                    .output()
            } else {
                Command::new("sh")
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

    // After every command, re-detect the branch: cwd may have moved (cd),
    // or the command may have switched branches (`git checkout`). Refresh
    // the file-browser listing too — a passthrough command may have
    // created or removed files in the cwd.
    state.refresh_branch();
    state.refresh_browser_files();
}

fn main() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT]),
        ..Default::default()
    };

    let state_path =
        save_data::default_path().unwrap_or_else(|| PathBuf::from(save_data::FILENAME));
    let state = State::load(state_path).unwrap_or_default();

    eframe::run_native("Shell", options, Box::new(|_cc| Ok(Box::new(state)))).unwrap();
}
