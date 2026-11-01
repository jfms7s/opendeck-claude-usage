//! Where the plugin finds the user's files.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `$HOME`, or `/` when it is unset - every home-relative default path
/// starts here.
pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()))
}

/// `$XDG_STATE_HOME` when absolute (per the XDG spec), else the platform's
/// place for app state: `~/.local/state` on Linux, `~/Library/Application
/// Support` on macOS.
pub fn state_base(xdg_state_home: Option<OsString>, home: &Path, macos: bool) -> PathBuf {
    xdg_state_home
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            if macos {
                home.join("Library/Application Support")
            } else {
                home.join(".local/state")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_dir_per_platform() {
        let abs = Some(OsString::from("/xdg/state"));
        let rel = Some(OsString::from("relative/state"));
        // An absolute $XDG_STATE_HOME wins on both platforms.
        assert_eq!(
            state_base(abs.clone(), Path::new("/home/jf"), false),
            PathBuf::from("/xdg/state")
        );
        assert_eq!(
            state_base(abs, Path::new("/Users/jf"), true),
            PathBuf::from("/xdg/state")
        );
        // Otherwise each platform's convention; a relative one is ignored.
        assert_eq!(
            state_base(None, Path::new("/home/jf"), false),
            PathBuf::from("/home/jf/.local/state")
        );
        assert_eq!(
            state_base(rel, Path::new("/home/jf"), false),
            PathBuf::from("/home/jf/.local/state")
        );
        assert_eq!(
            state_base(None, Path::new("/Users/jf"), true),
            PathBuf::from("/Users/jf/Library/Application Support")
        );
    }
}
