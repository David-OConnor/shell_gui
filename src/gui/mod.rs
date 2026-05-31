use egui::{
    Align, Button, CentralPanel, Color32, ComboBox, DragValue, Layout, ScrollArea, TextEdit, Ui,
    vec2,
};

use crate::State;

pub const WINDOW_WIDTH: f32 = 1200.;
pub const WINDOW_HEIGHT: f32 = 1000.;

pub(in crate::gui) const ROW_SPACING: f32 = 12.;
pub(in crate::gui) const COL_SPACING: f32 = 12.;

/// Display directory bookmarks, as a column.
fn dir_bookmarks(state: &mut State, ui: &mut Ui) {
    ui.label("Dir bookmarks");
    ui.separator();

    for (i, bm) in state.dir_bookmarks.iter().enumerate() {
        ui.label(format!("{i}: {}", bm.display()));
    }
}

/// Display recent directories we've executed a command from, in a colum.
fn dir_history(state: &mut State, ui: &mut Ui) {
    ui.label("Dir history");
    ui.separator();

    for (i, bm) in state.recent_dirs.iter().enumerate() {
        ui.label(format!("{i}: {}", bm.path.display()));
    }
}

/// Display recent commands entered, in a column.
fn cmd_history(state: &mut State, ui: &mut Ui) {
    ui.label("Cmd history");
    ui.separator();

    for (i, item) in state.history.iter().enumerate() {
        ui.label(format!("{i}: {}", item.text));
    }
}

/// Equivalent to the bulk of content in a CLI/ternimal: Stdout/err etc.
fn term_out(state: &mut State, ui: &mut Ui) {
    for item in &state.ui.out {
        // todo A/R
    }
}

/// Equivalent to the entry line of a CLI/terminal.
fn term_in(state: &mut State, ui: &mut Ui) {
    ui.text_edit_multiline(&mut state.ui.cli_input);
}

pub fn draw(state: &mut State, ui: &mut Ui) {
    CentralPanel::default().show_inside(ui, |ui| {
        dir_bookmarks(state, ui);
        dir_history(state, ui);
        cmd_history(state, ui);

        term_out(state, ui);
        term_in(state, ui);

        ui.label("Compositions");
    });

    // handle_file_dialogs(state, ui);
}
