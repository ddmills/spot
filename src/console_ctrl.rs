//! Stopping the audio when the console window is closed.
//!
//! Quitting with `q` or Ctrl-C goes through the app's own shutdown, but
//! clicking the terminal's X does not: Windows delivers `CTRL_CLOSE_EVENT` to
//! the process and, absent a handler, simply tears it down. That leaves a radio
//! station playing — the shutdown never runs, and the process can sit wedged in
//! its runtime teardown with the audio device still open long after the window
//! is gone.
//!
//! The handler runs on a thread Windows creates for it, with no tokio runtime
//! under it, so it cannot go through the command channel the way the normal
//! quit does. It talks straight to the radio thread instead and then ends the
//! process itself.

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::core::BOOL;

/// `TRUE`, spelled out rather than imported: the constant lives behind the
/// `Win32_Foundation` feature and this is the only thing in spot that would
/// need it.
const HANDLED: BOOL = 1;
const NOT_HANDLED: BOOL = 0;

/// Windows can deliver a second event while the first is being handled — an
/// impatient user clicking the X twice is exactly that case. Only the first
/// does the work.
static HANDLING: AtomicBool = AtomicBool::new(false);

/// Install the handler. A failure is not worth reporting: it costs only the
/// close-button path.
pub fn install() {
    unsafe {
        SetConsoleCtrlHandler(Some(handler), HANDLED);
    }
}

unsafe extern "system" fn handler(event: u32) -> BOOL {
    match event {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT
        | CTRL_SHUTDOWN_EVENT => {}
        // Not ours; let the next handler, and then the default one, see it.
        _ => return NOT_HANDLED,
    }
    if HANDLING.swap(true, Ordering::SeqCst) {
        return HANDLED;
    }

    // The only teardown that matters. Everything else spot holds dies with the
    // process; the audio device is the one thing that outlives it.
    crate::radio::player::stop_all_audio();
    // Cheap insurance for the paths where the window is not actually going
    // away, so the shell is not handed back in raw mode. A no-op if the TUI
    // never started.
    ratatui::restore();

    // Exit here rather than returning and letting Windows do it. On a close
    // event Windows allows a few seconds and then terminates the process, but
    // termination is exactly what fails to finish — and Ctrl-C is not a close
    // event at all, so returning would resume a spot whose audio has just been
    // stopped underneath it.
    std::process::exit(0)
}
