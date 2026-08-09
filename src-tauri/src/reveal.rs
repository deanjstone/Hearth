// "Reveal in file manager" for a handful of settings actions (skills dir,
// data dir, log file) — Phase 7, tracking issue #27. Electron's
// `shell.openPath`/`shell.showItemInFolder` have no bundled Rust equivalent;
// this app targets WSL2/Linux only (per spec #26's scope decision), so
// `xdg-open` on the containing directory is the portable choice across
// desktop environments. There's no cross-DE way to select a specific file
// inside its folder the way `showItemInFolder` does on Windows/macOS, so a
// log-file reveal opens its parent directory rather than selecting the file
// — an accepted, minor UX difference for this MVP.

use std::path::Path;
use std::process::Command;

pub fn reveal_dir(dir: &Path) -> Result<(), String> {
    Command::new("xdg-open")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn reveal_file(file: &Path) -> Result<(), String> {
    let dir = file.parent().unwrap_or(file);
    reveal_dir(dir)
}
