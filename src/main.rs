mod save_data;
mod tasks;
mod util;
mod gui;

use std::{
    env, io,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use crate::gui::{WINDOW_HEIGHT, WINDOW_WIDTH};

// Display this many history items at a time.
const DISP_HIST_LEN: usize = 20;

const DIVIDER: &str = "----------";

struct HistoryItem {
    pub text: String,
    pub dt: DateTime<Utc>,
}

struct RecentDir {
    pub path: PathBuf,
    pub dt: DateTime<Utc>,
}

// todo: Instead of storing these Arc<Mutex>>s, perhaps we do it some other way; this is due
// todo: due to how Rustyline expects it.
struct State {
    /// The current text the user has typed, pasted etc.
    pub cli_input: String,
    /// Cached.
    pub home: Option<PathBuf>,
    /// Shared with the Ctrl+H / arrow-key handlers, which render pages of
    /// recent commands without holding `State`.
    pub history: Vec<HistoryItem>,
    /// This initializes to env::current_dir, but is then managed from within
    /// this program.
    pub cwd: PathBuf,
    /// User-controlled list of directory bookmarks that can be easily
    /// navigated to. Shared with the readline key handler (Ctrl+B), which
    /// is why it lives behind an Arc<Mutex<_>>.
    pub dir_bookmarks: Vec<PathBuf>,
    /// Paths we've execute commands from. Works in a similar way to bookmarks.
    pub recent_dirs: Vec<RecentDir>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            home: util::get_home(),
            history: Vec::new(),
            cwd: env::current_dir().unwrap_or_default(),
            dir_bookmarks: Vec::new(),
            recent_dirs: Vec::new(),
        }
    }
}

impl State {
    /// This defines what the general prompt looks like. Its adorning characters let the user know they're in this shell.
    fn prompt(&self) -> String {
        // Mark the directory with a leading `*` when it's bookmarked.
        let bookmarked = self.dir_bookmarks;

        let star = if bookmarked { "*" } else { "" };
        format!("S {star}{} $ ", self.cwd.display())
    }

    /// Persist user-controlled state (bookmarks + recent dirs) to the given
    /// file. Called after every mutation of either list. Locks bookmarks
    /// before recent_dirs — keep this order consistent across all callers
    /// to avoid lock-order deadlocks.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        save_data::save_state(&self.dir_bookmarks, &self.recent_dirs, path)
    }

    /// Restore state from disk, returning a fresh `State` with that data.
    /// A missing file is treated as "no saved state" and yields the default
    /// `State::new()` values (not an error).
    pub fn load(path: &Path) -> io::Result<Self> {
        let (bookmarks, recent_dirs) = save_data::load_state(path)?;

        Ok(Self {
            home: util::get_home(),
            history: Vec::new(),
            cwd: env::current_dir().unwrap_or_default(),
            dir_bookmarks,
            recent_dirs,
        })
    }
}

impl eframe::App for State {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        gui::draw(self, ui);
    }
}

/// Render a path as `~/relative` when it lives under the home directory;
/// otherwise use the absolute form. Uses forward slashes after the tilde for
/// consistency with the rest of the shell.
fn render_with_tilde(p: &Path, home: Option<&Path>) -> String {
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

impl ShellHelper {
    fn render(&self, p: &Path) -> String {
        render_with_tilde(p, self.home.as_deref())
    }
}

/// Split a line into word ranges (byte start, byte end), treating quoted
/// regions as part of the surrounding word so that spaces inside `"..."` or
/// `'...'` don't break a token apart.
fn tokenize_words(line: &str) -> Vec<(usize, usize)> {
    let mut words = Vec::new();
    let mut quote: Option<char> = None;
    let mut word_start: Option<usize> = None;

    for (idx, ch) in line.char_indices() {
        match quote {
            Some(q) => {
                if ch == q {
                    quote = None;
                }
                if word_start.is_none() {
                    word_start = Some(idx);
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    if word_start.is_none() {
                        word_start = Some(idx);
                    }
                } else if ch.is_whitespace() {
                    if let Some(s) = word_start.take() {
                        words.push((s, idx));
                    }
                } else if word_start.is_none() {
                    word_start = Some(idx);
                }
            }
        }
    }
    if let Some(s) = word_start {
        words.push((s, line.len()));
    }
    words
}

/// Which paginated list the arrow keys currently page through. Set when the
/// user opens one of the lists (Ctrl+H / Ctrl+R / Alt+B); consulted by the
/// shared Left/Right handler.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NavKind {
    History,
    RecentDirs,
    Bookmarks,
}

/// Tracks the user's position when paging through one of the lists with
/// the arrow keys.
struct NavState {
    /// `None` until a list is opened; then the kind of list being paged.
    active: Option<NavKind>,
    /// 0 = most recent page (newest items, shown at the bottom).
    page: usize,
}

impl NavState {
    fn new() -> Self {
        Self {
            active: None,
            page: 0,
        }
    }
}

/// Total pages needed to show `total` items at `per_page` items per page.
/// Returns 1 when empty so the renderer can still show a "Page 1/1" frame.
fn page_count(total: usize, per_page: usize) -> usize {
    if total == 0 {
        1
    } else {
        total.div_ceil(per_page)
    }
}

/// Render one page of a list: header with paging hint + usage hint, a
/// page of rows, and a closing divider. Page 0 = the last `per_page`
/// items (newest at the bottom). Rows are labelled with their absolute
/// index into `items`, so the displayed number lines up with the
/// corresponding `<cmd> <number>` invocation.
fn render_page<T>(
    title: &str,
    usage_hint: &str,
    empty_msg: &str,
    items: &[T],
    page: usize,
    per_page: usize,
    mut format_row: impl FnMut(usize, &T) -> String,
) -> String {
    let total = items.len();
    let pages = page_count(total, per_page);
    let page = page.min(pages - 1);

    let mut msg = format!(
        "\n{title}  (← older page  → newer page).  {usage_hint}.  Page {}/{}:\n",
        page + 1,
        pages
    );
    msg.push_str(DIVIDER);
    msg.push('\n');

    if total == 0 {
        msg.push_str(empty_msg);
        msg.push('\n');
    } else {
        let end = total - page * per_page;
        let start = end.saturating_sub(per_page);
        for i in start..end {
            msg.push_str(&format_row(i, &items[i]));
            msg.push('\n');
        }
    }
    msg.push_str(DIVIDER);
    msg.push_str("\n\n");
    msg
}

fn render_history(history: &[HistoryItem], page: usize) -> String {
    render_page(
        "History",
        "Use `his <number>` to run; e.g. `his 4`",
        "(no history)",
        history,
        page,
        DISP_HIST_LEN,
        |i, item| format!("{i}:  {}", item.text),
    )
}

fn render_recent_dirs(
    recent: &[RecentDir],
    bookmarks: &[PathBuf],
    home: Option<&Path>,
    page: usize,
) -> String {
    render_page(
        "Recent directories",
        "Use `cd <number>` to go; e.g. `cd 4`",
        "(no recent directories)",
        recent,
        page,
        DISP_HIST_LEN,
        |i, r| {
            let star = if bookmarks.contains(&r.path) { "*" } else { "" };
            format!("{i}:  {star}{}", render_with_tilde(&r.path, home))
        },
    )
}

fn render_bookmarks(bookmarks: &[PathBuf], home: Option<&Path>, page: usize) -> String {
    render_page(
        "Bookmarks",
        "Use `del bm <number>` to delete; e.g. `del bm 4`",
        "(no bookmarks)",
        bookmarks,
        page,
        DISP_HIST_LEN,
        |i, bm| format!("{i}:  {}", render_with_tilde(bm, home)),
    )
}

/// Record `cwd` in the recent-dirs list. If the path is already present we
/// remove the old entry and push a fresh one to the end, so the list stays
/// deduped and the newest entry sits at the bottom of the display.
fn record_recent_dir(recent: &Arc<Mutex<Vec<RecentDir>>>, cwd: &Path) {
    if let Ok(mut list) = recent.lock() {
        list.retain(|r| r.path != cwd);
        list.push(RecentDir {
            path: cwd.to_path_buf(),
            dt: Utc::now(),
        });
    }
}

/// Runs one command line. Returns false if the shell should exit.
fn run_command(state: &mut State, state_path: &Path, input: &str) -> bool {
    let input = input.trim();
    if input.is_empty() {
        return true;
    }

    // Split into command + remainder for built-in dispatch.
    let (cmd, args) = match input.find(char::is_whitespace) {
        Some(i) => (&input[..i], input[i..].trim()),
        None => (input, ""),
    };

    // `his`/`hist <n>` re-runs a previous history item. Handle it before
    // recording the meta-invocation so the user's history stays focused on
    // the resolved command (which the recursive call below will record).
    if cmd == "his" || cmd == "hist" {
        match args.parse::<usize>() {
            Ok(idx) => {
                let resolved = state
                    .history
                    .and_then(|h| h.get(idx).map(|item| item.text.clone()));
                match resolved {
                    Some(text) => {
                        println!("> {text}");
                        return run_command(state, state_path, &text);
                    }
                    None => eprintln!("{cmd}: no history item at index {idx}"),
                }
            }
            Err(_) => eprintln!("{cmd}: usage: {cmd} <number>"),
        }
        return true;
    }

    if let Ok(mut hist) = state.history.lock() {
        hist.push(HistoryItem {
            text: input.to_string(),
            dt: Utc::now(),
        });
    }

    // Track directories we've run real commands from (everything except `cd`),
    // so Ctrl+R / `cd <number>` can jump back to them.
    if cmd != "cd" {
        let cwd = state.cwd.clone();
        record_recent_dir(&state.recent_dirs, &cwd);
        if let Err(e) = state.save(state_path) {
            eprintln!("warning: failed to save recent dirs: {e}");
        }
    }

    match cmd {
        "exit" | "quit" => return false,

        "sync" => {
            let message = args.trim().trim_matches('"');

            if message.is_empty() {
                eprintln!("sync: commit message required, e.g. sync \"my commit message\"");
            } else {
                let steps: [&[&str]; 3] = [&["add", "."], &["commit", "-am", message], &["push"]];

                for step in steps {
                    match Command::new("git").args(step).status() {
                        Ok(status) if !status.success() => {
                            eprintln!("sync: `git {}` failed", step.join(" "));
                            break;
                        }
                        Err(e) => {
                            eprintln!("sync: failed to run git: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        // On linux, this is likely the same as the system `cat` command, but it works on Windows.
        // Another approach may be to only apply this branch on Windows.
        "cat" => {
            let target = util::path_from_args(state, args);
            tasks::cat(&target);
        }

        "del" => {
            // `del bm <number>`: delete a bookmark by its displayed index
            // (the numbers shown by the Alt+B bookmark list).
            let (sub, rest) = match args.find(char::is_whitespace) {
                Some(i) => (&args[..i], args[i..].trim()),
                None => (args, ""),
            };

            match sub {
                "bm" => match rest.parse::<usize>() {
                    Ok(idx) => {
                        let mut removed = None;
                        match state.dir_bookmarks.lock() {
                            Ok(mut list) => {
                                if idx < list.len() {
                                    removed = Some(list.remove(idx));
                                } else {
                                    eprintln!(
                                        "del bm: no bookmark at index {idx} (have {})",
                                        list.len()
                                    );
                                }
                            }
                            Err(_) => eprintln!("del bm: bookmark list lock poisoned"),
                        }
                        if let Some(path) = removed {
                            println!("Deleted bookmark: {}", path.display());
                            if let Err(e) = state.save(state_path) {
                                eprintln!("del bm: failed to save bookmarks: {e}");
                            }
                        }
                    }
                    Err(_) => {
                        eprintln!("del bm: expected a number, e.g. `del bm 4`");
                    }
                },
                "" => eprintln!("del: usage: del bm <number>"),
                other => eprintln!("del: unknown target `{other}` (expected `bm`)"),
            }
        }

        "cd" => {
            // `cd <number>` (with nothing else after) jumps to a recent
            // directory by its Ctrl+R index. Anything else is resolved as a
            // normal path/bookmark argument.
            let target = if let Ok(idx) = args.parse::<usize>() {
                let resolved = state
                    .recent_dirs
                    .lock()
                    .ok()
                    .and_then(|list| list.get(idx).map(|r| r.path.clone()));
                match resolved {
                    Some(p) => Some(p),
                    None => {
                        eprintln!("cd: no recent directory at index {idx}");
                        None
                    }
                }
            } else {
                Some(util::path_from_args(state, args))
            };

            if let Some(target) = target {
                match env::set_current_dir(&target) {
                    Ok(_) => state.cwd = env::current_dir().unwrap_or(target),
                    Err(e) => eprintln!("cd: {e}"),
                }
            }
        }

        // `bm <number>`: jump to the bookmark at that Alt+B index. Mirrors
        // `cd <number>` but indexes into the bookmark list instead of
        // recent_dirs.
        "bm" => match args.parse::<usize>() {
            Ok(idx) => {
                let resolved = state
                    .dir_bookmarks
                    .lock()
                    .ok()
                    .and_then(|list| list.get(idx).cloned());
                match resolved {
                    Some(target) => match env::set_current_dir(&target) {
                        Ok(_) => state.cwd = env::current_dir().unwrap_or(target),
                        Err(e) => eprintln!("bm: {e}"),
                    },
                    None => eprintln!("bm: no bookmark at index {idx}"),
                }
            }
            Err(_) => eprintln!("bm: usage: bm <number>"),
        },

        // Everything else: Pass through to the system shell (e.g. the one which we launched this
        // application from)
        _ => {
            let result = if cfg!(windows) {
                // Powershell 7+; we will assume Windows users have this.
                // -NoProfile/-NoLogo skip loading the user's $PROFILE and the
                // startup banner, which together dominate pwsh's cold-start
                // time. Each command spawns a fresh process, so this shaves
                // ~200ms off every passthrough command.
                Command::new("pwsh")
                    .args(["-NoProfile", "-NoLogo", "-Command", input])
                    .status()
            } else {
                Command::new("sh").args(["-c", input]).status()
            };
            if let Err(e) = result {
                eprintln!("shell: {e}");
            }
        }
    }

    true
}

fn main() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([WINDOW_WIDTH, WINDOW_HEIGHT]),
        ..Default::default()
    };

    let mut state = State::default();

    eframe::run_native("Shell", options, Box::new(|_cc| Ok(Box::new(state)))).unwrap();
}
