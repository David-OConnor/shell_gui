mod gui;
mod tasks;

use std::{
    env, io,
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};

use shell::{RecentDir, get_home, path_from_args, save_data};

use crate::gui::{WINDOW_HEIGHT, WINDOW_WIDTH};

pub struct HistoryItem {
    pub text: String,
    pub dt: DateTime<Utc>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum OutType {
    Prompt,
    StdOut,
    StdErr,
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
    pub cli_input: String,
    pub out: Vec<OutItem>,
    /// Set when something programmatic should pull keyboard focus to the
    /// input box on the next frame (e.g. just after running a command).
    pub focus_input: bool,
}

impl Default for StateUi {
    fn default() -> Self {
        Self {
            cli_input: String::new(),
            out: Vec::new(),
            focus_input: true,
        }
    }
}

pub struct State {
    pub ui: StateUi,
    /// Cached.
    pub home: Option<PathBuf>,
    pub history: Vec<HistoryItem>,
    /// This initializes to env::current_dir, but is then managed from within
    /// this program.
    pub cwd: PathBuf,
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
        Self {
            ui: StateUi::default(),
            home: get_home(),
            history: Vec::new(),
            cwd: env::current_dir().unwrap_or_default(),
            dir_bookmarks: Vec::new(),
            recent_dirs: Vec::new(),
            state_path: save_data::default_path()
                .unwrap_or_else(|| PathBuf::from(save_data::FILENAME)),
        }
    }
}

impl State {
    /// Prompt prefix used when echoing a command into the output pane.
    pub fn prompt(&self) -> String {
        let bookmarked = self.dir_bookmarks.contains(&self.cwd);
        let star = if bookmarked { "*" } else { "" };
        format!("{star}{}", self.cwd.display())
    }

    pub fn save(&self) -> io::Result<()> {
        save_data::save_state(&self.dir_bookmarks, &self.recent_dirs, &self.state_path)
    }

    pub fn load(path: PathBuf) -> io::Result<Self> {
        let (bookmarks, recent_dirs) = save_data::load_state(&path)?;

        Ok(Self {
            ui: StateUi::default(),
            home: get_home(),
            history: Vec::new(),
            cwd: env::current_dir().unwrap_or_default(),
            dir_bookmarks: bookmarks,
            recent_dirs,
            state_path: path,
        })
    }

    pub fn push_out(&mut self, text: impl Into<String>, type_: OutType) {
        self.ui.out.push(OutItem {
            text: text.into(),
            type_,
            dt: Utc::now(),
        });
    }

    /// Toggle the current dir in the bookmark list. Returns the message we
    /// surfaced to the user, so callers can echo it into the output pane.
    pub fn toggle_bookmark_cwd(&mut self) -> String {
        let cwd = self.cwd.clone();
        if let Some(pos) = self.dir_bookmarks.iter().position(|p| p == &cwd) {
            self.dir_bookmarks.remove(pos);
            let _ = self.save();
            format!("Removed bookmark: {}", cwd.display())
        } else {
            self.dir_bookmarks.push(cwd.clone());
            let _ = self.save();
            format!("Added a bookmark: {}", cwd.display())
        }
    }

    /// `cd` to `target` and update cwd. Pushes a diagnostic on failure.
    pub fn change_dir(&mut self, target: PathBuf) {
        match env::set_current_dir(&target) {
            Ok(_) => self.cwd = env::current_dir().unwrap_or(target),
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
    // what they ran (mirrors how a normal terminal scrolls).
    let prompt = state.prompt();
    state.push_out(format!("{prompt}{input}"), OutType::Prompt);

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

    state.history.push(HistoryItem {
        text: input.to_string(),
        dt: Utc::now(),
    });

    // Track directories we've run real commands from (everything except `cd`).
    if cmd != "cd" {
        let cwd = state.cwd.clone();
        record_recent_dir(&mut state.recent_dirs, &cwd);
        if let Err(e) = state.save() {
            state.push_out(
                format!("warning: failed to save recent dirs: {e}"),
                OutType::StdErr,
            );
        }
    }

    match cmd {
        "exit" | "quit" => {
            state.push_out("(use the window close button to exit)", OutType::StdErr);
        }

        "sync" => {
            let message = args.trim().trim_matches('"');
            if message.is_empty() {
                state.push_out(
                    "sync: commit message required, e.g. sync \"my commit message\"",
                    OutType::StdErr,
                );
            } else {
                let steps: [&[&str]; 3] = [&["add", "."], &["commit", "-am", message], &["push"]];
                for step in steps {
                    let result = Command::new("git")
                        .args(step)
                        .current_dir(&state.cwd)
                        .output();
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
                            if !out.status.success() {
                                state.push_out(
                                    format!("sync: `git {}` failed", step.join(" ")),
                                    OutType::StdErr,
                                );
                                break;
                            }
                        }
                        Err(e) => {
                            state.push_out(
                                format!("sync: failed to run git: {e}"),
                                OutType::StdErr,
                            );
                            break;
                        }
                    }
                }
            }
        }

        "cat" => {
            let target =
                path_from_args(state.home.as_deref(), &state.cwd, &state.dir_bookmarks, args);
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
            let target = if let Ok(idx) = args.parse::<usize>() {
                match state.recent_dirs.get(idx).map(|r| r.path.clone()) {
                    Some(p) => Some(p),
                    None => {
                        state.push_out(
                            format!("cd: no recent directory at index {idx}"),
                            OutType::StdErr,
                        );
                        None
                    }
                }
            } else {
                Some(path_from_args(
                    state.home.as_deref(),
                    &state.cwd,
                    &state.dir_bookmarks,
                    args,
                ))
            };

            if let Some(target) = target {
                state.change_dir(target);
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
                    .current_dir(&state.cwd)
                    .output()
            } else {
                Command::new("sh")
                    .args(["-c", input])
                    .current_dir(&state.cwd)
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
