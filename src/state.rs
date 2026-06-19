use std::{
    env, io,
    path::{Path, PathBuf},
};

use chrono::Utc;
use shell::{
    BrowserFile, HistoryItem, NavState, OpenTabs, PanelVis, RecentDir, RemoteTerminal, WindowSize,
    apply_completion, complete_cd_path, current_branch, get_home, read_browser_files, save_data,
    truncate_branch,
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

    /// Per-tab browser-style back/forward navigation stacks (parallel to
    /// `State::cwd`). `nav_back` holds dirs we can return to (most recent on
    /// top); `nav_forward` holds dirs we can re-visit after going Back.
    /// Populated whenever the cwd changes via [State::change_dir]; not
    /// persisted (a fresh session starts with empty stacks).
    pub nav_back: Vec<Vec<PathBuf>>,
    pub nav_forward: Vec<Vec<PathBuf>>,
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
            nav_back: vec![Vec::new()],
            nav_forward: vec![Vec::new()],
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
    /// Last observed window size, refreshed every frame from the viewport so
    /// `save` persists the size the user left the window at. `None` until the
    /// first frame reports a size (and on a fresh file with no saved value).
    pub window_size: Option<WindowSize>,
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
            window_size: None,
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
        // Persist the current tab layout (each tab's cwd, in order, plus the
        // active index) so reopening the GUI restores the same tabs.
        let open_tabs = OpenTabs {
            paths: self.cwd.clone(),
            active: self.ui.active_tab,
        };
        save_data::save_state(
            &self.dir_bookmarks,
            &self.recent_dirs,
            &self.history,
            &self.remote_terminals,
            &self.ui.panel_vis,
            self.window_size,
            &open_tabs,
            &self.state_path,
        )
    }

    pub fn load(path: PathBuf) -> io::Result<Self> {
        let loaded = save_data::load_state(&path)?;

        // Restore the saved tabs. Drop any whose directory no longer exists
        // (it may have been deleted or renamed since the last run); if none
        // remain — or the file predates the feature — fall back to a single
        // tab at the current directory.
        let mut cwd: Vec<PathBuf> = loaded
            .open_tabs
            .paths
            .into_iter()
            .filter(|p| p.is_dir())
            .collect();
        if cwd.is_empty() {
            cwd.push(env::current_dir().unwrap_or_default());
        }

        // Clamp the saved active index to the (possibly shrunken) tab list and
        // point the process at that tab's cwd so the first command and the
        // initial file listing match the selected tab.
        let active_tab = loaded.open_tabs.active.min(cwd.len() - 1);
        let _ = env::set_current_dir(&cwd[active_tab]);

        let n = cwd.len();
        let branch = cwd.iter().map(|c| current_branch(c)).collect();

        let mut ui = StateUi::default();
        ui.panel_vis = loaded.panel_vis;
        ui.active_tab = active_tab;
        ui.cli_input = vec![String::new(); n];
        ui.out = (0..n).map(|_| Vec::new()).collect();
        ui.history_nav = (0..n).map(|_| NavState::new()).collect();
        ui.nav_back = (0..n).map(|_| Vec::new()).collect();
        ui.nav_forward = (0..n).map(|_| Vec::new()).collect();

        let mut s = Self {
            ui,
            home: get_home(),
            history: loaded.history,
            cwd,
            dir_bookmarks: loaded.bookmarks,
            recent_dirs: loaded.recent_dirs,
            remote_terminals: loaded.remote_terminals,
            browser_files: Vec::new(),
            state_path: path,
            branch,
            window_size: loaded.window_size,
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
        self.ui.nav_back.push(Vec::new());
        self.ui.nav_forward.push(Vec::new());
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
        self.ui.nav_back.remove(idx);
        self.ui.nav_forward.remove(idx);
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

    /// Raw cwd change for the active tab, without touching the back/forward
    /// stacks. Returns `true` if the cwd actually changed. Pushes a
    /// diagnostic on failure.
    fn navigate_to(&mut self, target: PathBuf) -> bool {
        let before = self.cwd().to_path_buf();
        match env::set_current_dir(&target) {
            Ok(_) => {
                let new_cwd = env::current_dir().unwrap_or(target);
                let i = self.ui.active_tab;
                self.cwd[i] = new_cwd;
                self.refresh_cwd_history_filter();
                self.refresh_branch();
                self.refresh_browser_files();
                self.cwd() != before
            }
            Err(e) => {
                self.push_out(format!("cd: {e}"), OutType::StdErr);
                false
            }
        }
    }

    /// `cd` to `target` and update the active tab's cwd. This is the "normal"
    /// navigation path (typed `cd`, clicking a bookmark/recent dir/folder,
    /// the Up button): on success it records the previous dir on the Back
    /// stack and clears the Forward stack, just like a web/file browser.
    pub fn change_dir(&mut self, target: PathBuf) {
        let before = self.cwd().to_path_buf();
        if self.navigate_to(target) {
            let i = self.ui.active_tab;
            self.ui.nav_back[i].push(before);
            self.ui.nav_forward[i].clear();
        }
    }

    /// Navigate to the parent of the active tab's cwd. No-op at a filesystem
    /// root. Records history like any other navigation.
    pub fn nav_up(&mut self) {
        if let Some(parent) = self.cwd().parent() {
            let parent = parent.to_path_buf();
            self.change_dir(parent);
        }
    }

    /// Back button: return to the most recent dir on the Back stack, pushing
    /// the current dir onto the Forward stack so it can be re-visited.
    pub fn nav_back(&mut self) {
        let i = self.ui.active_tab;
        let Some(target) = self.ui.nav_back[i].pop() else {
            return;
        };
        let before = self.cwd().to_path_buf();
        if self.navigate_to(target) {
            self.ui.nav_forward[i].push(before);
        }
    }

    /// Forward button: re-visit the dir we last went Back from.
    pub fn nav_forward(&mut self) {
        let i = self.ui.active_tab;
        let Some(target) = self.ui.nav_forward[i].pop() else {
            return;
        };
        let before = self.cwd().to_path_buf();
        if self.navigate_to(target) {
            self.ui.nav_back[i].push(before);
        }
    }
}

impl eframe::App for State {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Track the current window size each frame so `save` (which runs on
        // every command) and `on_exit` persist whatever size the user has
        // dragged the window to. `inner_rect` is the content area in logical
        // points — what we hand back to `with_inner_size` on the next launch.
        if let Some(rect) = ui.ctx().input(|i| i.viewport().inner_rect) {
            let size = rect.size();
            if size.x > 0.0 && size.y > 0.0 {
                self.window_size = Some(WindowSize {
                    x: size.x,
                    y: size.y,
                });
            }
        }
        gui::draw(self, ui);
    }

    /// Persist on close so a resize made without running any command (which
    /// would otherwise be the only thing that triggers a save) still sticks.
    /// Disambiguated to the inherent `save` — `eframe::App` also has a `save`.
    fn on_exit(&mut self) {
        let _ = State::save(self);
    }
}
