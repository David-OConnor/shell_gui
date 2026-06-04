use std::path::PathBuf;

use egui::{CentralPanel, Color32, Frame, Key, Panel, RichText, ScrollArea, TextEdit, Ui};

/// Renders a single-line "Search..." box that fills the available width and
/// triggers `on_changed` when the buffer is edited. Pulled out so all three
/// sidebars look identical.
fn search_box(ui: &mut Ui, buf: &mut String, id_salt: &str) -> bool {
    ui.add(
        TextEdit::singleline(buf)
            .hint_text("Search...")
            .desired_width(f32::INFINITY)
            .id_salt(id_salt),
    )
    .changed()
}

use crate::{OutType, State, ansi, render_with_tilde, run_command};

pub const WINDOW_WIDTH: f32 = 1400.;
pub const WINDOW_HEIGHT: f32 = 900.;

const SIDE_COL_WIDTH: f32 = 240.;
const RIGHT_COL_WIDTH: f32 = 280.;
const CWD_HIST_COL_WIDTH: f32 = 240.;

const COLOR_PROMPT: Color32 = Color32::from_rgb(220, 220, 90);
const COLOR_STDOUT: Color32 = Color32::from_rgb(210, 210, 210);
const COLOR_STDERR: Color32 = Color32::from_rgb(240, 110, 110);
// Light green to match the CLI's `his N` highlight.
const COLOR_HIS: Color32 = Color32::from_rgb(120, 240, 120);

/// Action a list-panel button asked us to take. Collected during rendering
/// (where we only hold `&State`'s fields immutably via iteration) and
/// applied afterwards, so the borrow checker is happy.
enum ListAction {
    ChangeDir(PathBuf),
    RemoveBookmark(usize),
    FillInput(String),
    /// Re-run the history item at this index in its original working dir,
    /// without changing the GUI's CWD. Delegates to the `hisd` built-in.
    RunHistoryInDir(usize),
}

/// Display directory bookmarks as a clickable column. Clicking a row cd's
/// to that bookmark; the `x` removes it from the list. The search box
/// filters by case-insensitive substring against the full path.
fn dir_bookmarks(state: &mut State, ui: &mut Ui) {
    ui.heading("Bookmarks");

    if ui.button("+ bookmark current dir").clicked() {
        let msg = state.toggle_bookmark_cwd();
        state.push_out(msg, OutType::StdOut);
    }

    if search_box(ui, &mut state.ui.bookmarks_search, "bookmarks_search") {
        state.refresh_bookmarks_filter();
    }
    ui.separator();

    let home = state.home.clone();
    let mut action: Option<ListAction> = None;

    ScrollArea::vertical()
        .id_salt("bookmarks_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for &i in &state.ui.bookmarks_filter {
                let Some(bm) = state.dir_bookmarks.get(i) else {
                    continue;
                };
                ui.horizontal(|ui| {
                    let label = format!("{i}: {}", render_with_tilde(bm, home.as_deref()));
                    if ui.button(label).clicked() {
                        action = Some(ListAction::ChangeDir(bm.clone()));
                    }
                    if ui.small_button("x").clicked() {
                        action = Some(ListAction::RemoveBookmark(i));
                    }
                });
            }
        });

    apply_action(state, action);
}

/// Display recent directories we've executed a command from, as a clickable
/// column. Newest first. Stars mark dirs that are also bookmarked. The
/// search box filters by case-insensitive substring against the full path.
fn dir_history(state: &mut State, ui: &mut Ui) {
    ui.heading("Recent dirs");

    if search_box(ui, &mut state.ui.recent_dirs_search, "recent_dirs_search") {
        state.refresh_recent_dirs_filter();
    }
    ui.separator();

    let home = state.home.clone();
    let mut action: Option<ListAction> = None;

    ScrollArea::vertical()
        .id_salt("recent_dirs_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Render newest-first so the most-recently-used dirs are at the top.
            for &i in state.ui.recent_dirs_filter.iter().rev() {
                let Some(r) = state.recent_dirs.get(i) else {
                    continue;
                };
                let star = if state.dir_bookmarks.contains(&r.path) {
                    "*"
                } else {
                    ""
                };
                let label = format!("{i}: {star}{}", render_with_tilde(&r.path, home.as_deref()));
                if ui.button(label).clicked() {
                    action = Some(ListAction::ChangeDir(r.path.clone()));
                }
            }
        });

    apply_action(state, action);
}

/// Display commands previously run in the active tab's cwd. Newest first.
/// Same click behavior as the global history panel.
fn cwd_cmd_history(state: &mut State, ui: &mut Ui) {
    ui.heading("In this dir");

    if search_box(
        ui,
        &mut state.ui.cwd_history_search,
        "cwd_history_search",
    ) {
        state.refresh_cwd_history_filter();
    }
    ui.separator();

    let mut action: Option<ListAction> = None;

    ScrollArea::vertical()
        .id_salt("cwd_cmd_history_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for &i in state.ui.cwd_history_filter.iter().rev() {
                let Some(item) = state.history.get(i) else {
                    continue;
                };
                ui.horizontal(|ui| {
                    let label = format!("{i}: {}", item.text);
                    if ui.button(label).clicked() {
                        action = Some(ListAction::FillInput(item.text.clone()));
                    }
                });
            }
        });

    apply_action(state, action);
}

/// Display recent commands entered, newest first. Clicking the label copies
/// the command into the input box so the user can edit or run it; the
/// "In dir" button re-runs it in the directory it was originally run from.
fn cmd_history(state: &mut State, ui: &mut Ui) {
    ui.heading("Command history");

    if search_box(ui, &mut state.ui.history_search, "history_search") {
        state.refresh_history_filter();
    }
    ui.separator();

    let mut action: Option<ListAction> = None;

    ScrollArea::vertical()
        .id_salt("cmd_history_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Newest first.
            for &i in state.ui.history_filter.iter().rev() {
                let Some(item) = state.history.get(i) else {
                    continue;
                };
                ui.horizontal(|ui| {
                    let label = format!("{i}: {}", item.text);
                    if ui.button(label).clicked() {
                        action = Some(ListAction::FillInput(item.text.clone()));
                    }
                    // Last path component, capped at 20 chars so long folder
                    // names don't blow out the row width.
                    let leaf = item
                        .dir
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| item.dir.display().to_string());
                    let leaf = if leaf.chars().count() > 20 {
                        let truncated: String = leaf.chars().take(20).collect();
                        truncated
                    } else {
                        leaf
                    };
                    if ui.small_button(format!("In {leaf}")).clicked() {
                        action = Some(ListAction::RunHistoryInDir(i));
                    }
                });
            }
        });

    apply_action(state, action);
}

fn apply_action(state: &mut State, action: Option<ListAction>) {
    match action {
        Some(ListAction::ChangeDir(p)) => state.change_dir(p),
        Some(ListAction::RemoveBookmark(i)) => {
            if i < state.dir_bookmarks.len() {
                let path = state.dir_bookmarks.remove(i);
                state.refresh_bookmarks_filter();
                state.push_out(
                    format!("Deleted bookmark: {}", path.display()),
                    OutType::StdOut,
                );
                let _ = state.save();
            }
        }
        Some(ListAction::FillInput(s)) => {
            let i = state.ui.active_tab;
            state.ui.cli_input[i] = s;
            state.ui.focus_input = true;
        }
        Some(ListAction::RunHistoryInDir(i)) => {
            // Reuse the `hisd` built-in so the spawn / output-capture path
            // stays in one place (run_command in main.rs).
            run_command(state, &format!("hisd {i}"));
        }
        None => {}
    }
}

/// The "terminal" output pane: scrolls, sticks to the bottom so the latest
/// line stays in view, and colours rows by stream kind.
fn term_out(state: &mut State, ui: &mut Ui) {
    ScrollArea::vertical()
        .id_salt("term_out_scroll")
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
            let mono_font = egui::TextStyle::Monospace.resolve(ui.style());
            let active = state.ui.active_tab;
            for item in &state.ui.out[active] {
                let default_color = match item.type_ {
                    OutType::Prompt => COLOR_PROMPT,
                    OutType::StdOut => COLOR_STDOUT,
                    OutType::StdErr => COLOR_STDERR,
                };
                // Trim a trailing newline so blank rows don't accumulate
                // (Command output usually ends with one).
                let text = item.text.trim_end_matches('\n');
                let segments = ansi::parse(text);
                let job = ansi::to_layout_job(&segments, default_color, mono_font.clone());
                ui.label(job);
            }
        });
}

/// The input row: prompt label on the left, single-line edit on the right.
/// Enter submits. When the user is recalling a history entry via Up/Down or
/// a recent dir via Left/Right, a green ` his N` / ` cd N` label is
/// inserted between the cwd and the `$`.
fn term_in(state: &mut State, ui: &mut Ui) {
    let active = state.ui.active_tab;
    let (his_cursor, cd_cursor) = state.active_nav_cursors();
    let cwd_part = state.prompt(&shell::NavState::new());
    ui.horizontal(|ui| {
        ui.label(RichText::new(cwd_part).color(COLOR_PROMPT).monospace());
        if let Some(i) = his_cursor {
            ui.label(
                RichText::new(format!("his {i}"))
                    .color(COLOR_HIS)
                    .monospace(),
            );
        }
        if let Some(i) = cd_cursor {
            ui.label(
                RichText::new(format!("cd {i}"))
                    .color(COLOR_HIS)
                    .monospace(),
            );
        }
        ui.label(RichText::new("$").color(COLOR_PROMPT).monospace());

        let response = ui.add(
            TextEdit::singleline(&mut state.ui.cli_input[active])
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );

        if state.ui.focus_input {
            response.request_focus();
            state.ui.focus_input = false;
        }

        // Arrow-key recall: ↑/↓ walks the global history list, ←/→ walks
        // recent dirs. Up/Down are safe to consume unconditionally (the
        // single-line TextEdit doesn't use them), but Left/Right only
        // steal the keystroke when the buffer is empty or a cd recall is
        // already active — otherwise they stay as normal caret movement.
        if response.has_focus() {
            let up = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowUp));
            let down = ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowDown));
            if up {
                state.history_nav(true);
            } else if down {
                state.history_nav(false);
            } else {
                let cd_active = cd_cursor.is_some() || state.ui.cli_input[active].is_empty();
                if cd_active {
                    let left = ui
                        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft));
                    let right = ui
                        .input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::ArrowRight));
                    if left {
                        state.recent_dir_nav(true);
                    } else if right {
                        state.recent_dir_nav(false);
                    }
                }
            }
        }

        let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if submitted {
            let input = std::mem::take(&mut state.ui.cli_input[active]);
            run_command(state, &input);
            state.ui.focus_input = true;
        }
    });
}

/// Top-of-window tab strip. Each tab labels itself with its cwd's leaf
/// folder; clicking selects, `x` closes (disabled when only one tab is
/// left), and `+` appends a fresh tab inheriting the active tab's cwd.
fn tabs_bar(state: &mut State, ui: &mut Ui) {
    // Snapshot the data we need so the inner closure doesn't fight the
    // borrow checker with the mutable `state` we mutate at the end.
    let active = state.ui.active_tab;
    let n = state.cwd.len();
    let home = state.home.clone();
    let labels: Vec<String> = state
        .cwd
        .iter()
        .map(|p| {
            p.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| render_with_tilde(p, home.as_deref()))
        })
        .collect();

    let mut to_select: Option<usize> = None;
    let mut to_close: Option<usize> = None;
    let mut add_new = false;

    ui.horizontal(|ui| {
        for (i, leaf) in labels.iter().enumerate() {
            let text = format!("{i}: {leaf}");
            if ui.selectable_label(i == active, text).clicked() {
                to_select = Some(i);
            }
            if n > 1 && ui.small_button("x").on_hover_text("Close tab").clicked() {
                to_close = Some(i);
            }
            ui.separator();
        }
        if ui.button("+").on_hover_text("New tab").clicked() {
            add_new = true;
        }
    });

    if let Some(i) = to_select {
        state.select_tab(i);
    }
    if let Some(i) = to_close {
        state.close_tab(i);
    }
    if add_new {
        state.add_tab();
    }
}

pub fn draw(state: &mut State, ui: &mut Ui) {
    Panel::top("tabs_panel")
        .resizable(false)
        .show_inside(ui, |ui| {
            tabs_bar(state, ui);
        });

    Panel::left("bookmarks_panel")
        .resizable(true)
        .default_size(SIDE_COL_WIDTH)
        .show_inside(ui, |ui| {
            dir_bookmarks(state, ui);
        });

    Panel::left("recent_dirs_panel")
        .resizable(true)
        .default_size(SIDE_COL_WIDTH)
        .show_inside(ui, |ui| {
            dir_history(state, ui);
        });

    Panel::right("cmd_history_panel")
        .resizable(true)
        .default_size(RIGHT_COL_WIDTH)
        .show_inside(ui, |ui| {
            cmd_history(state, ui);
        });

    Panel::right("cwd_cmd_history_panel")
        .resizable(true)
        .default_size(CWD_HIST_COL_WIDTH)
        .show_inside(ui, |ui| {
            cwd_cmd_history(state, ui);
        });

    // Black backgrounds on the terminal panes so they read as a terminal,
    // not as a regular widget area.
    let term_frame = Frame::default()
        .fill(Color32::BLACK)
        .inner_margin(egui::Margin::same(6));

    // Pin the input row to the bottom of the central area, with the
    // scrolling output pane filling the remaining space above it.
    Panel::bottom("term_in_panel")
        .resizable(false)
        .frame(term_frame)
        .show_inside(ui, |ui| {
            term_in(state, ui);
        });

    CentralPanel::default()
        .frame(term_frame)
        .show_inside(ui, |ui| {
            term_out(state, ui);
        });
}
