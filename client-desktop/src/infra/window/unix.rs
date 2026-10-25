//! Window helpers outside Windows: Slint shows and hides the window itself, there is no native drop target,
//! and files are chosen with the desktop's own dialog (`zenity` on GNOME-like desktops, `kdialog` on KDE).

use std::path::PathBuf;
use std::process::Command;

use crate::infra::logger;

/// Slint hides the window (see [`super::NATIVE_HIDE`]); nothing to do natively.
pub fn hide_window_by_title(_window_title: &str) {}

/// No native drop target outside Windows (see [`super::FILE_DROP_SUPPORTED`]).
pub fn register_window_file_drop<F>(_window_title: &str, _callback: F) -> bool
where
    F: Fn(Vec<PathBuf>) + Send + Sync + 'static,
{
    false
}

/// Shows a multi-select "Choose Files to Send" dialog; empty when cancelled or when no dialog tool exists.
pub fn pick_files_dialog() -> Vec<PathBuf> {
    let attempts: [(&str, &[&str]); 2] = [
        ("zenity", &["--file-selection", "--multiple", "--separator=\n", "--title=Choose Files to Send"]),
        ("kdialog", &["--getopenfilename", "--multiple", "--separate-output", ".", "--title", "Choose Files to Send"]),
    ];
    for (tool, args) in attempts {
        match Command::new(tool).args(args).output() {
            Ok(out) => {
                // Exit code 1 = cancelled.
                return String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(PathBuf::from)
                    .collect();
            }
            Err(_) => continue,
        }
    }
    logger::warn("window", "No file dialog found: install zenity or kdialog to pick files to send.");
    Vec::new()
}
