//! Where the editor was, so it comes back there: the project or state file
//! that was open and the canvas's pan and zoom. Saved when a document is
//! opened, saved somewhere new or started (New), and again on exit for the
//! view — after a restart (the remote's "Restart focused app", `ccectl
//! restart-app`), at login, or when reopened, a launch opens on it.
//!
//! **A restart replays the same command line**, so `cce-graph a/` would
//! always come back on `a/` even after the user opened `b/`. The file
//! therefore also records the path the command line named (`launched`): the
//! same command line again is this window reopening, and the saved document
//! wins; a command line naming another path is a new request, and it wins
//! (with the default view — the saved one belongs to another document).
//! Restoring only opens the document; it never writes one.
//!
//! `cce_state_dir()/graph/state.json`, written whole to a temporary file and
//! renamed over, so a crash mid-write leaves the last one. Vault mode
//! (`--vault`) keeps none of this: its camera fits a live layout.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The document open: a project directory or a bare `.kdl` state file.
    pub open: Option<PathBuf>,
    /// The path the command line named when this was saved, if any.
    pub launched: Option<PathBuf>,
    /// The canvas's grid origin, in window pixels.
    pub origin: (f32, f32),
    /// Node width over the unzoomed width.
    pub zoom: f32,
}

/// This run's command-line path; unset until `initial` (so tests save nothing).
static LAUNCHED: OnceLock<Option<PathBuf>> = OnceLock::new();

fn path() -> PathBuf {
    cce_ui::config::cce_state_dir().join("graph").join("state.json")
}

/// The command line's path, made absolute — a restart replays the same cwd,
/// but a relaunch from another directory naming `a/` asks for a different `a/`.
fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The place to reopen, if this launch is one that reopens: record the
/// command line's path and compare it with the saved one.
pub fn initial(argv_path: Option<&Path>) -> Option<Saved> {
    let launched = argv_path.map(absolute);
    let _ = LAUNCHED.set(launched.clone());
    let saved = std::fs::read_to_string(path()).ok().and_then(|s| serde_json::from_str::<Saved>(&s).ok());
    choose(launched.as_deref(), saved, |p| p.exists())
}

/// The saved place when this command line is the one it was saved under and
/// its document is still there; otherwise `None`, and the command line (or
/// the default document) is opened as before.
fn choose(launched: Option<&Path>, saved: Option<Saved>, exists: impl Fn(&Path) -> bool) -> Option<Saved> {
    saved.filter(|s| s.launched.as_deref() == launched && s.open.as_deref().is_some_and(&exists))
}

/// Record where the editor is.
pub fn save(open: Option<&Path>, origin: (f32, f32), zoom: f32) {
    let Some(launched) = LAUNCHED.get() else { return };
    let saved = Saved { open: open.map(absolute), launched: launched.clone(), origin, zoom };
    let path = path();
    let Some(dir) = path.parent() else { return };
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&tmp, serde_json::to_vec(&saved).unwrap_or_default()))
        .and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        eprintln!("[cce-graph] cannot save the open project to {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(open: &str, launched: Option<&str>) -> Saved {
        Saved { open: Some(open.into()), launched: launched.map(PathBuf::from), origin: (10.0, 20.0), zoom: 1.5 }
    }

    fn all_exist(_: &Path) -> bool {
        true
    }

    #[test]
    fn the_same_command_line_reopens_where_it_was() {
        // Plain launch, then a plain relaunch: the saved document.
        let s = saved("/p/b", None);
        assert_eq!(choose(None, Some(s.clone()), all_exist), Some(s));
        // Launched on a/, opened b/, restarted with the same argv: b/.
        let s = saved("/p/b", Some("/p/a"));
        assert_eq!(choose(Some(Path::new("/p/a")), Some(s.clone()), all_exist), Some(s));
    }

    #[test]
    fn a_command_line_naming_another_path_wins() {
        let a = Path::new("/p/a");
        assert_eq!(choose(Some(Path::new("/p/c")), Some(saved("/p/b", Some("/p/a"))), all_exist), None);
        assert_eq!(choose(Some(a), Some(saved("/p/b", None)), all_exist), None);
        // A plain launch after one that named a path is a different request too.
        assert_eq!(choose(None, Some(saved("/p/b", Some("/p/a"))), all_exist), None);
    }

    #[test]
    fn nothing_saved_or_nothing_there_falls_back() {
        assert_eq!(choose(None, None, all_exist), None);
        assert_eq!(choose(Some(Path::new("/p/a")), None, all_exist), None);
        // The saved document is gone: open as the command line says.
        assert_eq!(choose(None, Some(saved("/p/b", None)), |_| false), None);
        // A new graph never saved anywhere has nothing to reopen.
        let unsaved = Saved { open: None, ..saved("/p/b", None) };
        assert_eq!(choose(None, Some(unsaved), all_exist), None);
    }
}
