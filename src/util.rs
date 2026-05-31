//! Misc utility functionality.

use std::{env, path::PathBuf};

use crate::State;

/// Resolve a `cd`/`cat`-style path argument against the shell's state:
/// expands `~`/`~/...` to the home dir, treats real paths literally, and
/// falls back to a case-insensitive prefix match against bookmarked
/// directories. Infallible — unresolvable cases degrade to the literal
/// `cwd`-joined path rather than erroring.
pub fn path_from_args(state: &State, args: &str) -> PathBuf {
    if args.is_empty() || args == "~" {
        // cd with no args (or a bare `~`) goes home; fall back to the
        // current directory if we couldn't resolve a home dir.
        state.home.clone().unwrap_or_else(|| state.cwd.clone())
    } else if let Some(rest) = args.strip_prefix("~/").or_else(|| args.strip_prefix("~\\")) {
        state
            .home
            .as_ref()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| state.cwd.join(args))
    } else {
        // Try the literal path first so real subdirs / absolute paths
        // keep their normal meaning. If it isn't a directory, fall
        // back to a prefix-match against bookmarked directories
        // (matched against the bookmark's final path component,
        // case-insensitive).
        let literal = state.cwd.join(args);
        if literal.is_dir() {
            literal
        } else {
            let needle = args.to_lowercase();
            let bookmark_match = state.dir_bookmarks.lock().ok().and_then(|list| {
                list.iter()
                    .find(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| n.to_lowercase().starts_with(&needle))
                            .unwrap_or(false)
                    })
                    .cloned()
            });
            bookmark_match.unwrap_or(literal)
        }
    }
}

pub fn get_home() -> Option<PathBuf> {
    // Resolve the home directory once; used for bare `cd` and for
    // expanding a leading `~` / `~/...` in the argument.
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}
