//! This module contains utilities; e.g. simple things certain command can delegate to.

use std::{fs, io, path::Path};

/// Read a file into a `String`. Used by the `cat` builtin to populate the
/// output pane.
pub fn read_file(path: &Path) -> io::Result<String> {
    fs::read_to_string(path)
}
