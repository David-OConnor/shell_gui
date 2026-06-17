use std::{
    env, io,
    path::{Path, PathBuf},
};

use chrono::Utc;
use shell::{
    BrowserFile, HistoryItem, NavState, PanelVis, RecentDir, RemoteTerminal, apply_completion,
    complete_cd_path, current_branch, get_home, read_browser_files, save_data, truncate_branch,
};

use crate::{OutItem, OutType, gui};

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
    pub panel_vis: PanelVis,
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
            panel_vis: PanelVis::default(),
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
    pub remote_terminals: Vec<RemoteTerminal>,
    /// In the current dir. Note persistent, unlike some of our other lists.
    pub browser_files: Vec<BrowserFile>,
    /// Where bookmarks + recent dirs are persisted.
    pub state_path: PathBuf,
    /// Cached git branch for each tab's cwd (parallel to `cwd`). `None`
    /// when that tab's cwd isn't inside a repo. Refreshed by
    /// `refresh_branch` after every command, cd, tab switch, or new tab.
    pub branch: Vec<Option<String>>,
}

impl Default for State {
    fn default() -> Self {
        let cwd = env::current_dir().unwrap_or_default();
        let branch = current_branch(&cwd);
        let mut s = Self {
            ui: StateUi::default(),
            home: get_home(),
            history: Vec::new(),
            cwd: vec![cwd],
            dir_bookmarks: Vec::new(),
            recent_dirs: Vec::new(),
            remote_terminals: Vec::new(),
            browser_files: Vec::new(),
            state_path: save_data::default_path()
                .unwrap_or_else(|| PathBuf::from(save_data::FILENAME)),
            branch: vec![branch],
        };
        s.refresh_all_filters();
        s.refresh_browser_files();
        s
    }
}

impl State {
    /// Just the `{star}{cwd}` portion of the prompt, used by the live
    /// input row which renders the branch + nav indicators as separately
    /// coloured labels (see `gui::term_in`).
    pub fn cwd_display(&self) -> String {
        let cwd = self.cwd();
        let bookmarked = self.dir_bookmarks.iter().any(|p| p.as_path() == cwd);
        let star = if bookmarked { "*" } else { "" };
        format!("{star}{}", cwd.display())
    }

    /// Full prompt prefix used when echoing a command into the output
    /// pane. Embeds an ANSI magenta escape around the branch slot so the
    /// output pane's ANSI parser colours it the same as the live input
    /// row's branch label. `nav` adds a ` his N` / ` cd N` indicator
    /// after the branch when a recall is active (see [NavState]).
    pub fn prompt(&self, nav: &NavState) -> String {
        let i = self.ui.active_tab;
        let branch_part = self.branch[i]
            .as_deref()
            .map(|b| format!(" \x1b[95mbranch: {}\x1b[0m", truncate_branch(b, 10)))
            .unwrap_or_default();
        format!(
            "{}{}{}{}",
            self.cwd_display(),
            branch_part,
            nav.his_indicator(),
            nav.cd_indicator(),
        )
    }

    /// Re-detect the git branch for the active tab's cwd. Called after
    /// every command (a `git checkout` may have switched branches), after
    /// `cd`, tab creation, and tab selection.
    pub fn refresh_branch(&mut self) {
        let i = self.ui.active_tab;
        self.branch[i] = current_branch(&self.cwd[i]);
    }

    /// Re-read the active tab's cwd into `browser_files`. Called after
    /// every command (a passthrough command may have created/removed
    /// files), after `cd`, tab creation, and tab selection — same trigger
    /// points as [refresh_branch].
    pub fn refresh_browser_files(&mut self) {
        let cwd = self.cwd().to_path_buf();
        self.browser_files = read_browser_files(&cwd);
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
            &self.remote_terminals,
            &self.ui.panel_vis,
            &self.state_path,
        )
    }

    pub fn load(path: PathBuf) -> io::Result<Self> {
        let loaded = save_data::load_state(&path)?;
        let cwd = env::current_dir().unwrap_or_default();
        let branch = current_branch(&cwd);

        let mut ui = StateUi::default();
        ui.panel_vis = loaded.panel_vis;
        let mut s = Self {
            ui,
            home: get_home(),
            history: loaded.history,
            cwd: vec![cwd],
            dir_bookmarks: loaded.bookmarks,
            recent_dirs: loaded.recent_dirs,
            remote_terminals: loaded.remote_terminals,
            browser_files: Vec::new(),
            state_path: path,
            branch: vec![branch],
        };
        s.refresh_all_filters();
        s.refresh_browser_files();
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
        // New tab inherits the active tab's branch (same cwd → same repo).
        let inherited_branch = self.branch[self.ui.active_tab].clone();

        self.cwd.push(new_cwd);
        self.ui.cli_input.push(String::new());
        self.ui.out.push(Vec::new());
        self.ui.history_nav.push(NavState::new());
        self.branch.push(inherited_branch);

        self.ui.active_tab = self.cwd.len() - 1;
        self.ui.focus_input = true;

        self.refresh_cwd_history_filter();
        self.refresh_browser_files();
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
        self.branch.remove(idx);

        if self.ui.active_tab >= self.cwd.len() {
            self.ui.active_tab = self.cwd.len() - 1;
        } else if idx < self.ui.active_tab {
            self.ui.active_tab -= 1;
        }

        let cwd = self.cwd().to_path_buf();
        let _ = env::set_current_dir(&cwd);

        self.refresh_cwd_history_filter();
        // Branch may have moved on if the closed tab ran a checkout while
        // hidden, or if the newly active tab is in a different repo.
        self.refresh_branch();
        self.refresh_browser_files();
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx >= self.cwd.len() || idx == self.ui.active_tab {
            return;
        }

        self.ui.active_tab = idx;
        let cwd = self.cwd().to_path_buf();
        let _ = env::set_current_dir(&cwd);

        self.refresh_cwd_history_filter();
        self.refresh_branch();
        self.refresh_browser_files();

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
                needle.is_empty() || p.display().to_string().to_lowercase().contains(&needle)
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
                    || r.path
                        .display()
                        .to_string()
                        .to_lowercase()
                        .contains(&needle)
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

        let result = self.ui.history_nav[i].step_cd(&self.recent_dirs, left, &live, |path| {
            format!("cd {}", crate::render_with_tilde(path, home.as_deref()))
        });

        if let Some(text) = result {
            self.ui.cli_input[i] = text;
        }
    }

    /// Complete the active input row using the shared `cd` path completer.
    pub fn autocomplete_input(&mut self) {
        let i = self.ui.active_tab;
        let live = self.ui.cli_input[i].clone();
        let Some(result) = complete_cd_path(
            &live,
            live.len(),
            self.cwd(),
            self.home.as_deref(),
            &self.dir_bookmarks,
        ) else {
            return;
        };
        if let Some(text) = apply_completion(&live, live.len(), &result) {
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
                self.refresh_branch();
                self.refresh_browser_files();
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
