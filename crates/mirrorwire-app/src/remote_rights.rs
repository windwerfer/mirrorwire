//! Whether the desktop's permission for the viewer's pen/touch and remote keys (RemoteDesktop
//! portal) is remembered, and forgetting it.
//!
//! The portal has no call to ask whether a permission was granted earlier, nor one to revoke it
//! (ScreenCast v6 / RemoteDesktop v2 spec). What MirrorWire can know is whether it holds a
//! restore token: the desktop hands one out only when the user allowed restoring the session
//! (KDE: "Allow restoring on future sessions"). Forgetting deletes those tokens, so the next
//! presenter start asks again; the desktop may keep its own record until it is removed there.

use anyhow::{Context, Result};
use mirrorwire_media::source::portal::input_token_file;
use mirrorwire_media::source::CaptureMode;
use std::path::{Path, PathBuf};

/// Restore token of the separate keys/pen session (used when capture and input cannot share
/// one portal session).
pub const SEPARATE_TOKEN_FILE: &str = "remote-desktop.token";

/// Every token file that carries pen/keys permission: the combined capture + input session
/// (Mirror and Extend) and the separate input session.
fn token_files(config_dir: &Path) -> Vec<PathBuf> {
    [
        input_token_file(CaptureMode::Mirror),
        input_token_file(CaptureMode::Extend),
        SEPARATE_TOKEN_FILE,
    ]
    .iter()
    .map(|name| config_dir.join(name))
    .collect()
}

/// True when this mode's combined capture + input session has a stored restore token: the
/// presenter then opens capture and input together at start, without a dialog.
pub fn combined_saved(config_dir: &Path, mode: CaptureMode) -> bool {
    non_empty(&config_dir.join(input_token_file(mode)))
}

fn non_empty(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
}

/// True when at least one non-empty pen/keys restore token is stored. An unreadable file
/// counts as not remembered.
pub fn remembered(config_dir: &Path) -> bool {
    token_files(config_dir).iter().any(|p| non_empty(p))
}

/// Delete every pen/keys restore token. Returns how many files were removed; missing files are
/// fine. The screen-only capture token is kept (it carries no input permission).
pub fn forget(config_dir: &Path) -> Result<usize> {
    let mut removed = 0;
    for path in token_files(config_dir) {
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("deleting {}", path.display())),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mirrorwire-rights-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn remembered_follows_the_token_files_and_forget_clears_them() {
        let dir = temp_dir();
        assert!(!remembered(&dir));
        assert_eq!(forget(&dir).unwrap(), 0);
        // An empty file is no permission.
        std::fs::write(dir.join(SEPARATE_TOKEN_FILE), " \n").unwrap();
        assert!(!remembered(&dir));
        assert!(!combined_saved(&dir, CaptureMode::Mirror));
        std::fs::write(dir.join(input_token_file(CaptureMode::Extend)), "tok").unwrap();
        assert!(remembered(&dir));
        // Per mode: the Extend token does not start Mirror with input.
        assert!(combined_saved(&dir, CaptureMode::Extend));
        assert!(!combined_saved(&dir, CaptureMode::Mirror));
        // The screen-only token is not an input permission and survives forgetting.
        let cast = dir.join(CaptureMode::Mirror.restore_token_file());
        std::fs::write(&cast, "screen").unwrap();
        assert_eq!(forget(&dir).unwrap(), 2);
        assert!(!remembered(&dir));
        assert!(cast.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
