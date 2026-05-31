//! Persistent application state. Currently the user's bookmark list plus
//! the recent-directories list, but the file format is line-based and
//! tagged so we can add more record types later without breaking existing
//! files.
//!
//! Format:
//!   # comments and blank lines are ignored
//!   BOOKMARK <absolute path>
//!   RECENT_DIR <rfc3339 timestamp> <absolute path>
//!
//! Unknown record types are silently skipped on load so older builds reading
//! a file written by a newer build don't choke.

use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};

use crate::RecentDir;

pub const FILENAME: &str = "shell_state.ss";

const BOOKMARK_TAG: &str = "BOOKMARK ";
const RECENT_DIR_TAG: &str = "RECENT_DIR ";

/// Where the state file lives by default: `<home>/shell_state.ss`. Falls back
/// to `None` if neither `USERPROFILE` nor `HOME` is set (rare).
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(|h| PathBuf::from(h).join(FILENAME))
}

/// Overwrite the state file with the given bookmark + recent-dir lists.
/// Creates parent directories as needed.
pub fn save_state(bookmarks: &[PathBuf], recent_dirs: &[RecentDir], path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let mut f = fs::File::create(path)?;
    writeln!(
        f,
        "# Shell state — auto-generated. Do not edit while shell is running."
    )?;

    for bm in bookmarks {
        writeln!(f, "{BOOKMARK_TAG}{}", bm.display())?;
    }
    for r in recent_dirs {
        // "<rfc3339> <path>" — rfc3339 has no spaces, so the path can be the
        // (possibly space-containing) tail.
        writeln!(
            f,
            "{RECENT_DIR_TAG}{} {}",
            r.dt.to_rfc3339(),
            r.path.display()
        )?;
    }

    Ok(())
}

/// Read the persistent state. A missing file is not an error — it just
/// means no saved state yet, so we return empty vecs.
pub fn load_state(path: &Path) -> io::Result<(Vec<PathBuf>, Vec<RecentDir>)> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(e) => return Err(e),
    };

    let mut bookmarks = Vec::new();
    let mut recent_dirs = Vec::new();

    for line in BufReader::new(file).lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(BOOKMARK_TAG) {
            bookmarks.push(PathBuf::from(rest));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(RECENT_DIR_TAG) {
            // Split on the first space: token 1 is the rfc3339 dt, the rest
            // is the path (which may itself contain spaces).
            if let Some(space) = rest.find(' ') {
                let (dt_str, path_str) = rest.split_at(space);
                let path_str = path_str.trim_start();
                if let Ok(dt) = DateTime::parse_from_rfc3339(dt_str) {
                    recent_dirs.push(RecentDir {
                        path: PathBuf::from(path_str),
                        dt: dt.with_timezone(&Utc),
                    });
                }
            }
            continue;
        }
        // Unknown tags are ignored on purpose for forward compatibility.
    }
    Ok((bookmarks, recent_dirs))
}
