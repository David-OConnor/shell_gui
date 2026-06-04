mod ansi;
mod gui;
mod tasks;

use std::{
    env, io,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};

use shell::commands::{self, OutKind};
use shell::{HistoryItem, NavState, RecentDir, get_home, path_from_args, save_data};

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

/// State specific to the GUI: the current input buffer, the captured terminal
/// output, and a "please focus the input next frame" flag we use after
/// running a command so the user can keep typing.
pub struct StateUi {
    /// By tab
    pub cli_input: Vec<String>,
    /// Outer: Per tab. Inner: Out items in that tab.
    pub out: Vec<Vec<OutItem>>,
    /// Which tab in `State::cwd` / `cli_input` / `out` is currently shown.
    /// Always a valid index into those parallel vectors; we maintain the
    /// invariant that there is at least one tab.
    pub active_tab: usize,
    /// Set when something programmatic should pull keyboard focus to the
    /// input box on the next frame (e.g. just after running a command).
    pub focus_input: bool,

    /// Search text for each sidebar panel. Empty = "show everything". Updated
    /// in-place by the panel's TextEdit.
    pub bookmarks_search: String,
    pub recent_dirs_search: String,
    pub history_search: String,
    pub cwd_history_search: String,

    /// Indices into the corresponding source list (`dir_bookmarks`,
    /// `recent_dirs`, `history`) that match the current search. Re-derived
    /// whenever the search text or the source list changes via the
    /// `refresh_*_filter` helpers, so the render loop can just iterate these
    /// indices without re-running the filter every frame.
    pub bookmarks_filter: Vec<usize>,
    pub recent_dirs_filter: Vec<usize>,
    pub history_filter: Vec<usize>,
    /// History indices restricted to the active tab's cwd. Refreshed on
    /// cwd changes, tab switches, and new history entries.
    pub cwd_history_filter: Vec<usize>,

    /// Per-tab arrow-key recall state: Up/Down walks `history`, Left/Right
    /// walks `recent_dirs`. See [NavState].
    pub history_nav: Vec<NavState>,
}

impl Default for StateUi {
    fn default() -> Self {
        Self {
            cli_input: vec![String::new()],
            out: vec![Vec::new()],
            active_tab: 0,
            focus_input: true,
            bookmarks_search: String::new(),
            recent_dirs_search: String::new(),
            history_search: String::new(),
            cwd_history_search: String::new(),
            bookmarks_filter: Vec::new(),
            recent_dirs_filter: Vec::new(),
            history_filter: Vec::new(),
            cwd_history_filter: Vec::new(),
            history_nav: vec![NavState::new()],
        }
    }
}

pub struct State {
    pub ui: StateUi,
    /// Cached.
    pub home: Option<PathBuf>,
    pub history: Vec<HistoryItem>,
    /// This initializes to env::current_dir, but is then managed from within
    /// this program. Per tab.
    pub cwd: Vec<PathBuf>,
    /// User-controlled list of directory bookmarks that can be easily
    /// navigated to.
    pub dir_bookmarks: Vec<PathBuf>,
    /// Paths we've execute commands from. Works in a similar way to bookmarks.
    pub recent_dirs: Vec<RecentDir>,
    /// Where bookmarks + recent dirs are persisted.
    pub state_path: PathBuf,
}

impl Default for State {
    fn default() -> Self {
        let mut s = Self {
            ui: StateUi::default(),
            home: get_home(),
            history: Vec::new(),
            cwd: vec![env::current_dir().unwrap_or_default()],
            dir_bookmarks: Vec::new(),
            recent_dirs: Vec::new(),
            state_path: save_data::default_path()
                .unwrap_or_else(|| PathBuf::from(save_data::FILENAME)),
        };
        s.refresh_all_filters();
        s
    }
}

impl State {
    /// Prompt prefix used when echoing a command into the output pane and
    /// for rendering the live input row. `nav` adds a ` his N` / ` cd N`
    /// indicator after the cwd when a recall is active (see [NavState]).
    pub fn prompt(&self, nav: &NavState) -> String {
        let cwd = self.cwd();
        let bookmarked = self.dir_bookmarks.iter().any(|p| p.as_path() == cwd);
        let star = if bookmarked { "*" } else { "" };
        format!(
            "{star}{}{}{}",
            cwd.display(),
            nav.his_indicator(),
            nav.cd_indicator(),
        )
    }

    /// Snapshot of the active tab's recall cursors. Returned by value so
    /// callers can build a prompt string without holding a borrow on
    /// `self.ui.history_nav`.
    pub fn active_nav_cursors(&self) -> (Option<usize>, Option<usize>) {
        let n = &self.ui.history_nav[self.ui.active_tab];
        (n.his_cursor, n.cd_cursor)
    }

    pub fn save(&self) -> io::Result<()> {
        save_data::save_state(
            &self.dir_bookmarks,
            &self.recent_dirs,
            &self.history,
            &self.state_path,
        )
    }

    pub fn load(path: PathBuf) -> io::Result<Self> {
        let (bookmarks, recent_dirs, history) = save_data::load_state(&path)?;

        let mut s = Self {
            ui: StateUi::default(),
            home: get_home(),
            history,
            cwd: vec![env::current_dir().unwrap_or_default()],
            dir_bookmarks: bookmarks,
            recent_dirs,
            state_path: path,
        };
        s.refresh_all_filters();
        Ok(s)
    }

    /// Active tab's cwd. Always valid: we maintain at least one tab.
    pub fn cwd(&self) -> &Path {
        &self.cwd[self.ui.active_tab]
    }

    pub fn push_out(&mut self, text: impl Into<String>, type_: OutType) {
        let i = self.ui.active_tab;
        self.ui.out[i].push(OutItem {
            text: text.into(),
            type_,
            dt: Utc::now(),
        });
    }

    /// Open a new tab inheriting the active tab's cwd. Switches focus to it.
    pub fn add_tab(&mut self) {
        let new_cwd = self.cwd().to_path_buf();
        self.cwd.push(new_cwd);
        self.ui.cli_input.push(String::new());
        self.ui.out.push(Vec::new());
        self.ui.history_nav.push(NavState::new());
        self.ui.active_tab = self.cwd.len() - 1;
        self.ui.focus_input = true;
        self.refresh_cwd_history_filter();
    }

    /// Close a tab. No-op if it's the last remaining one (we keep at least
    /// one tab open so the invariant holds).
    pub fn close_tab(&mut self, idx: usize) {
        if self.cwd.len() <= 1 || idx >= self.cwd.len() {
            return;
        }
        self.cwd.remove(idx);
        self.ui.cli_input.remove(idx);
        self.ui.out.remove(idx);
        self.ui.history_nav.remove(idx);
        if self.ui.active_tab >= self.cwd.len() {
            self.ui.active_tab = self.cwd.len() - 1;
        } else if idx < self.ui.active_tab {
            self.ui.active_tab -= 1;
        }
        let cwd = self.cwd().to_path_buf();
        let _ = env::set_current_dir(&cwd);
        self.refresh_cwd_history_filter();
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx >= self.cwd.len() || idx == self.ui.active_tab {
            return;
        }
        self.ui.active_tab = idx;
        let cwd = self.cwd().to_path_buf();
        let _ = env::set_current_dir(&cwd);
        self.refresh_cwd_history_filter();
        self.ui.focus_input = true;
    }

    /// Rebuild every cached filter. Used right after construction (so the
    /// first frame already has populated filter lists) and as a convenience
    /// when we don't know which list just changed.
    pub fn refresh_all_filters(&mut self) {
        self.refresh_bookmarks_filter();
        self.refresh_recent_dirs_filter();
        self.refresh_history_filter();
        self.refresh_cwd_history_filter();
    }

    /// History indices restricted to the active tab's cwd, then narrowed by
    /// the cwd-history search box.
    pub fn refresh_cwd_history_filter(&mut self) {
        let cwd = self.cwd().to_path_buf();
        let needle = self.ui.cwd_history_search.to_lowercase();
        self.ui.cwd_history_filter = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, h)| h.dir == cwd)
            .filter(|(_, h)| needle.is_empty() || h.text.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
    }

    /// Case-insensitive substring match against each bookmark's full
    /// rendered path. Empty search matches everything.
    pub fn refresh_bookmarks_filter(&mut self) {
        let needle = self.ui.bookmarks_search.to_lowercase();
        self.ui.bookmarks_filter = self
            .dir_bookmarks
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                needle.is_empty()
                    || p.display().to_string().to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
    }

    pub fn refresh_recent_dirs_filter(&mut self) {
        let needle = self.ui.recent_dirs_search.to_lowercase();
        self.ui.recent_dirs_filter = self
            .recent_dirs
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                needle.is_empty()
                    || r.path.display().to_string().to_lowercase().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
    }

    /// Searches just the command text (not the dir) — that's what the user
    /// typically remembers when re-running.
    pub fn refresh_history_filter(&mut self) {
        let needle = self.ui.history_search.to_lowercase();
        self.ui.history_filter = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, h)| needle.is_empty() || h.text.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
    }

    /// Toggle the current dir in the bookmark list. Returns the message we
    /// surfaced to the user, so callers can echo it into the output pane.
    pub fn toggle_bookmark_cwd(&mut self) -> String {
        let cwd = self.cwd().to_path_buf();
        let msg = if let Some(pos) = self.dir_bookmarks.iter().position(|p| p == &cwd) {
            self.dir_bookmarks.remove(pos);
            format!("Removed bookmark: {}", cwd.display())
        } else {
            self.dir_bookmarks.push(cwd.clone());
            format!("Added a bookmark: {}", cwd.display())
        };
        let _ = self.save();
        self.refresh_bookmarks_filter();
        msg
    }

    /// Walk the global history (all directories) for the active tab's input
    /// row. `up = true` moves to an older entry; `up = false` to a newer one.
    /// Delegates to [NavState::step_his] for the shared logic.
    pub fn history_nav(&mut self, up: bool) {
        let i = self.ui.active_tab;
        let live = self.ui.cli_input[i].clone();
        if let Some(text) = self.ui.history_nav[i].step_his(&self.history, up, &live) {
            self.ui.cli_input[i] = text;
        }
    }

    /// Walk recent-dirs for the active tab's input row. `left = true` moves
    /// to an older entry; `left = false` to a newer one. Loads a
    /// `cd <tilde-path>` command into the buffer so Enter goes there.
    pub fn recent_dir_nav(&mut self, left: bool) {
        let i = self.ui.active_tab;
        let live = self.ui.cli_input[i].clone();
        let home = self.home.clone();
        let result = self.ui.history_nav[i].step_cd(
            &self.recent_dirs,
            left,
            &live,
            |path| format!("cd {}", render_with_tilde(path, home.as_deref())),
        );
        if let Some(text) = result {
            self.ui.cli_input[i] = text;
        }
    }

    /// Reset the active tab's recall state back to the live input. Called
    /// after a command is submitted.
    pub fn reset_history_nav(&mut self) {
        let i = self.ui.active_tab;
        self.ui.history_nav[i].reset();
    }

    /// `cd` to `target` and update the active tab's cwd. Pushes a diagnostic
    /// on failure.
    pub fn change_dir(&mut self, target: PathBuf) {
        match env::set_current_dir(&target) {
            Ok(_) => {
                let new_cwd = env::current_dir().unwrap_or(target);
                let i = self.ui.active_tab;
                self.cwd[i] = new_cwd;
                self.refresh_cwd_history_filter();
            }
            Err(e) => self.push_out(format!("cd: {e}"), OutType::StdErr),
        }
    }
}

impl eframe::App for State {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        gui::draw(self, ui);
    }
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
            let target =
                path_from_args(state.home.as_deref(), state.cwd(), &state.dir_bookmarks, args);
            match tasks::read_file(&target) {
                Ok(text) => state.push_out(text, OutType::StdOut),
                Err(e) => state.push_out(
                    format!("cat: {}: {e}", target.display()),
                    OutType::StdErr,
                ),
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
