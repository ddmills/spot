//! Making a double-click behave like `spot.exe` typed into Windows Terminal.
//!
//! spot is a TUI: it wants a truecolor terminal, and the console Explorer hands
//! a double-clicked exe is whatever the machine's default terminal happens to
//! be - on Windows 10 that is the legacy host, which renders the palette and
//! the album art wrong. Rather than telling people to open a terminal and cd to
//! their downloads folder, spot notices it was started from Explorer and starts
//! itself again inside Windows Terminal.

use std::path::PathBuf;
use std::process::Command;

use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::GetClassNameW;

/// Set on the child so the relaunched copy can never bounce a second time, and
/// published in the README as the opt-out for anyone who wants spot in the
/// console they already have.
const OPT_OUT: &str = "SPOT_NO_RELAUNCH";

/// True if a Windows Terminal child was started and this process should exit.
///
/// Every check below is a reason to stay put, so anything unexpected - no
/// Windows Terminal, an API that will not answer, a spawn that fails - ends
/// with spot running normally in the console it was given.
pub fn relaunch_in_windows_terminal() -> bool {
    // WT_SESSION means we are already inside Windows Terminal, which is the
    // case on Windows 11, where it is the default terminal for a double-click.
    if std::env::var_os(OPT_OUT).is_some() || std::env::var_os("WT_SESSION").is_some() {
        return false;
    }
    if !owns_console_alone() || !console_is_legacy_host() {
        return false;
    }
    let Some(wt) = find_wt() else {
        return false;
    };
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };

    // -w -1 forces a new window: without it the tab is grafted onto whichever
    // Windows Terminal window happens to be open, yanking the user out of
    // whatever they were doing there.
    Command::new(wt)
        .args(["-w", "-1", "nt", "--title", "spot"])
        .arg(exe)
        .env(OPT_OUT, "1")
        .spawn()
        .is_ok()
}

/// Whether this process is the only one attached to its console, which is what
/// separates "double-clicked from Explorer" - a console created for us alone -
/// from "run from a shell", where the shell is attached too.
fn owns_console_alone() -> bool {
    let mut pids = [0u32; 2];
    // The return is the number of attached processes, which may exceed the
    // buffer; only "exactly one" matters, so a truncated count still answers
    // the question. Zero means no console at all, or a failure - either way,
    // not a double-click.
    let count = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    count == 1
}

/// Whether this console is the classic console host rather than the
/// pseudoconsole every modern terminal drives.
///
/// This is the check that decides whether relaunching is an improvement at
/// all. Windows 11 already hands a double-clicked exe to Windows Terminal, and
/// WT_SESSION is not set on that path - the terminal did not launch us, it
/// adopted our console - so without this spot would open a second, redundant
/// window on exactly the machines that needed no help. A pseudoconsole means
/// something modern is already drawing us; leave it alone.
fn console_is_legacy_host() -> bool {
    let hwnd = unsafe { GetConsoleWindow() };
    if hwnd.is_null() {
        return false;
    }
    let mut class = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
    if len <= 0 {
        return false;
    }
    String::from_utf16_lossy(&class[..len as usize]) == "ConsoleWindowClass"
}

fn find_wt() -> Option<PathBuf> {
    // The Store package installs an app-execution alias here and puts the
    // directory on PATH, but an Explorer session started before the install
    // carries a stale PATH, so look for the alias directly first.
    if let Some(local) = dirs::data_local_dir() {
        let alias = local.join("Microsoft").join("WindowsApps").join("wt.exe");
        if alias.exists() {
            return Some(alias);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("wt.exe"))
        .find(|candidate| candidate.exists())
}
