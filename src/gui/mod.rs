use std::path::PathBuf;

use egui::{CentralPanel, Color32, Frame, Key, Panel, RichText, ScrollArea, TextEdit, Ui};

use crate::{OutType, State, render_with_tilde, run_command};

pub const WINDOW_WIDTH: f32 = 1400.;
pub const WINDOW_HEIGHT: f32 = 900.;

const SIDE_COL_WIDTH: f32 = 240.;
const RIGHT_COL_WIDTH: f32 = 280.;

const COLOR_PROMPT: Color32 = Color32::from_rgb(220, 220, 90);
const COLOR_STDOUT: Color32 = Color32::from_rgb(210, 210, 210);
const COLOR_STDERR: Color32 = Color32::from_rgb(240, 110, 110);

/// Action a list-panel button asked us to take. Collected during rendering
/// (where we only hold `&State`'s fields immutably via iteration) and
/// applied afterwards, so the borrow checker is happy.
enum ListAction {
    ChangeDir(PathBuf),
    RemoveBookmark(usize),
    FillInput(String),
}

/// Display directory bookmarks as a clickable column. Clicking a row cd's
/// to that bookmark; the `x` removes it from the list.
fn dir_bookmarks(state: &mut State, ui: &mut Ui) {
    ui.heading("Bookmarks");

    if ui.button("+ bookmark current dir").clicked() {
        let msg = state.toggle_bookmark_cwd();
        state.push_out(msg, OutType::StdOut);
    }
    ui.separator();

    let home = state.home.clone();
    let mut action: Option<ListAction> = None;

    ScrollArea::vertical()
        .id_salt("bookmarks_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, bm) in state.dir_bookmarks.iter().enumerate() {
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
/// column. Newest first. Stars mark dirs that are also bookmarked.
fn dir_history(state: &mut State, ui: &mut Ui) {
    ui.heading("Recent dirs");
    ui.separator();

    let home = state.home.clone();
    let mut action: Option<ListAction> = None;

    ScrollArea::vertical()
        .id_salt("recent_dirs_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            // Render newest-first so the most-recently-used dirs are at the top.
            for (i, r) in state.recent_dirs.iter().enumerate().rev() {
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

/// Display recent commands entered, newest first. Clicking copies the
/// command into the input box so the user can edit or run it.
fn cmd_history(state: &mut State, ui: &mut Ui) {
    ui.heading("Command history");
    ui.separator();

    let mut action: Option<ListAction> = None;

    ScrollArea::vertical()
        .id_salt("cmd_history_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, item) in state.history.iter().enumerate().rev() {
                let label = format!("{i}: {}", item.text);
                if ui.button(label).clicked() {
                    action = Some(ListAction::FillInput(item.text.clone()));
                }
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
                state.push_out(
                    format!("Deleted bookmark: {}", path.display()),
                    OutType::StdOut,
                );
                let _ = state.save();
            }
        }
        Some(ListAction::FillInput(s)) => {
            state.ui.cli_input = s;
            state.ui.focus_input = true;
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
            for item in &state.ui.out {
                let color = match item.type_ {
                    OutType::Prompt => COLOR_PROMPT,
                    OutType::StdOut => COLOR_STDOUT,
                    OutType::StdErr => COLOR_STDERR,
                };
                // Trim a trailing newline so blank rows don't accumulate
                // (Command output usually ends with one).
                let text = item.text.trim_end_matches('\n');
                ui.label(RichText::new(text).color(color).monospace());
            }
        });
}

/// The input row: prompt label on the left, single-line edit on the right.
/// Enter submits.
fn term_in(state: &mut State, ui: &mut Ui) {
    let prompt = state.prompt();
    ui.horizontal(|ui| {
        ui.label(RichText::new(prompt).color(COLOR_PROMPT).monospace());

        let response = ui.add(
            TextEdit::singleline(&mut state.ui.cli_input)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );

        if state.ui.focus_input {
            response.request_focus();
            state.ui.focus_input = false;
        }

        let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        if submitted {
            let input = std::mem::take(&mut state.ui.cli_input);
            run_command(state, &input);
            state.ui.focus_input = true;
        }
    });
}

pub fn draw(state: &mut State, ui: &mut Ui) {
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
