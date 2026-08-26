//! The system clipboard, for the share controls.
//!
//! One handle for the life of the process. On X11 and Wayland the clipboard
//! holds no text of its own — it names a live window that will hand the text
//! over when something asks for it — so a handle dropped after the write takes
//! the copied link with it and the paste comes back empty.

use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use arboard::Clipboard;

fn handle() -> &'static Mutex<Option<Clipboard>> {
    static HANDLE: OnceLock<Mutex<Option<Clipboard>>> = OnceLock::new();
    HANDLE.get_or_init(|| Mutex::new(None))
}

/// Put `text` on the clipboard, opening the handle on first use.
///
/// A poisoned lock is recovered from rather than propagated: the only thing
/// under it is a clipboard handle, and refusing every later copy because one
/// write panicked helps nobody.
pub fn copy(text: &str) -> Result<()> {
    let mut guard = handle().lock().unwrap_or_else(|e| e.into_inner());
    let clipboard = match guard.as_mut() {
        Some(clipboard) => clipboard,
        None => guard.insert(Clipboard::new()?),
    };
    Ok(clipboard.set_text(text)?)
}

#[cfg(test)]
mod tests {
    /// Not part of the suite: it writes the real system clipboard, and there
    /// is no headless one to write instead.
    #[test]
    #[ignore]
    fn a_copy_reaches_the_system_clipboard() {
        let before = super::handle()
            .lock()
            .unwrap()
            .get_or_insert_with(|| arboard::Clipboard::new().unwrap())
            .get_text()
            .ok();
        super::copy("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC").unwrap();
        let read = super::handle()
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .get_text()
            .unwrap();
        assert_eq!(
            read,
            "https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC"
        );
        if let Some(before) = before {
            super::copy(&before).unwrap();
        }
    }
}
