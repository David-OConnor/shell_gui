//! This module contains utilities; e.g. simple things certain command can delegate to.

use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
};

/// Like the Linux cat command; outputs the contents of a file to stdout.
/// On any error (file missing, not readable, mid-stream read failure) it
/// prints a `cat: ...` diagnostic to stderr and returns, matching the
/// shell's other builtins.
pub fn cat(path: &Path) {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("cat: {}: {e}", path.display());
            return;
        }
    };

    for line in BufReader::new(file).lines() {
        match line {
            Ok(l) => println!("{l}"),
            Err(e) => {
                eprintln!("cat: {}: {e}", path.display());
                return;
            }
        }
    }
}
