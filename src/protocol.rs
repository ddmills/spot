//! Making a clicked Spotify link open in spot.
//!
//! Windows routes a URL scheme to whatever `HKCU\Software\Classes\<scheme>`
//! names, so opening links in spot means claiming `spotify:` from the desktop
//! app that normally holds it. That changes how the machine behaves outside
//! spot, so nothing here runs on its own.
//!
//! Three rules hold it to that, and the shape of this module is what enforces
//! them rather than care at the call sites:
//!
//! 1. [`register`] and [`unregister`] are the only functions that claim or
//!    release a scheme, and they are reached from two places — the command
//!    line and the Home row. A startup path may call [`status`], which only
//!    reads, and [`repair_path`], which can rewrite a path and nothing else.
//! 2. `protocol.json` is the consent. No file, no claim: a fresh copy of the
//!    exe on a new machine starts neutral, and [`repair_path`] returns without
//!    reading the registry at all.
//! 3. Claiming is confirmed before it happens — by the box the Home row opens,
//!    or by `--force` on the command line. Giving the schemes back is not:
//!    undo must never be harder than the act.
//!
//! Everything written lives under `HKEY_CURRENT_USER`, so none of it needs an
//! administrator and none of it reaches another account. That matters beyond
//! politeness: an elevated change would be one the user could not undo from
//! inside spot.

use anyhow::{Context, Result};
use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

use crate::config::{self, SavedProtocol};

/// spot's own scheme. Nothing else claims it, so taking it costs nobody
/// anything; it exists so spot has a name of its own to be reached by.
const OWN_SCHEME: &str = "spot";
/// The scheme that carries every Spotify link in circulation, and the one the
/// desktop app holds by default.
const SPOTIFY_SCHEME: &str = "spotify";

const CLASSES: &str = r"Software\Classes";

/// Windows' own default-apps answer, which outranks anything under `Classes`.
/// The hash beside it is checked by the shell and cannot be forged, so a
/// choice recorded here can only be changed by the user in Settings.
const USER_CHOICE: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\UrlAssociations\spotify\UserChoice";

/// What opens Spotify links on this machine now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    Spot,
    /// Another app, named as well as its command line allows.
    Other(String),
    Nobody,
}

/// The state of the claim, as read rather than as remembered.
#[derive(Debug, Clone)]
pub struct Registration {
    pub holder: Holder,
    /// Whether the user asked spot to hold the schemes. True without
    /// [`Holder::Spot`] means something took them back.
    pub consented: bool,
    /// The app Windows' default-apps setting names, when it names one. While
    /// this is set, nothing spot writes has any effect.
    pub user_choice: Option<String>,
}

impl Registration {
    /// Whether a click on a Spotify link reaches spot right now.
    pub fn in_force(&self) -> bool {
        self.holder == Holder::Spot && self.user_choice.is_none()
    }

    /// The line the Home row and the command line both say, stating what is
    /// true rather than what is on offer.
    pub fn describe(&self) -> String {
        if let Some(app) = &self.user_choice {
            return format!("Windows opens Spotify links in {app}");
        }
        match &self.holder {
            Holder::Spot => "Spotify links open in spot".to_string(),
            Holder::Other(app) => format!("Spotify links open in {app}"),
            Holder::Nobody => "nothing on this machine opens Spotify links".to_string(),
        }
    }
}

/// Read the claim. The only registry call a startup path may make.
pub fn status() -> Registration {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let command = open_command(&hkcu, SPOTIFY_SCHEME);
    let holder = match command.as_deref() {
        None => Holder::Nobody,
        Some(cmd) if is_this_exe(cmd) => Holder::Spot,
        Some(cmd) => Holder::Other(app_name(cmd)),
    };
    Registration {
        holder,
        consented: config::load_protocol().is_some(),
        user_choice: hkcu
            .open_subkey_with_flags(USER_CHOICE, KEY_READ)
            .ok()
            .and_then(|key| key.get_value::<String, _>("ProgId").ok())
            .filter(|progid| !progid.is_empty()),
    }
}

/// Read the claim into the Home row, and disarm it.
///
/// Called at startup and after the row acts. Reading rather than remembering
/// is the point: another app can take the scheme back between runs, and a row
/// that reported what spot last wrote would say the wrong thing.
pub fn refresh(st: &mut crate::app::state::AppState) {
    let now = status();
    st.links.in_force = now.in_force();
    st.links.status = now.describe();
    st.links.confirming = None;
}

/// Claim both schemes for this executable.
///
/// `force` answers for the user when another app already holds `spotify:`.
/// The caller is what asked — the command line by the flag, the Home row by
/// its confirm box — so this refuses rather than guessing.
pub fn register(force: bool) -> Result<Registration> {
    let before = status();
    if let Holder::Other(app) = &before.holder
        && !force
    {
        anyhow::bail!("{app} already opens Spotify links");
    }
    let exe = exe_path()?;

    // The backup first, and only when there is not one already: registering
    // twice must not overwrite the real previous holder with spot itself.
    if !before.consented {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        config::save_protocol(&SavedProtocol {
            previous_command: open_command(&hkcu, SPOTIFY_SCHEME),
            exe: exe.clone(),
        })
        .context("could not record what spot is about to replace")?;
    }

    claim(OWN_SCHEME, &exe)?;
    claim(SPOTIFY_SCHEME, &exe)?;
    Ok(status())
}

/// Give both schemes back, exactly as they were found.
pub fn unregister() -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let classes = hkcu
        .open_subkey_with_flags(CLASSES, KEY_READ | KEY_WRITE)
        .context("could not open the class registrations")?;

    match config::load_protocol().and_then(|saved| saved.previous_command) {
        // Put the displaced app back where it was.
        Some(previous) => {
            let (key, _) = classes
                .create_subkey(format!(r"{SPOTIFY_SCHEME}\shell\open\command"))
                .context("could not restore the previous handler")?;
            key.set_value("", &previous)?;
        }
        // Nothing held it, so the key spot made is the whole of what it added.
        None => {
            let _ = classes.delete_subkey_all(SPOTIFY_SCHEME);
        }
    }
    let _ = classes.delete_subkey_all(OWN_SCHEME);
    // Last: while this file exists the claim is still spot's to answer for.
    config::clear_protocol();
    Ok(())
}

/// Re-point an existing registration at this executable, and nothing else.
///
/// spot is one portable file that people move, and a registration naming a
/// path that no longer holds it opens nothing at all. This cannot add a
/// scheme, cannot change which app is displaced, and returns before reading
/// the registry unless the user has already asked for the claim.
pub fn repair_path() {
    let Some(saved) = config::load_protocol() else {
        return;
    };
    let Ok(exe) = exe_path() else {
        return;
    };
    if saved.exe == exe {
        return;
    }
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    for scheme in [OWN_SCHEME, SPOTIFY_SCHEME] {
        // Only where the stale path is still what the scheme names. Anything
        // else means another app has taken it, and re-pointing that would be a
        // claim rather than a repair.
        if let Some(cmd) = open_command(&hkcu, scheme)
            && command_exe(&cmd).eq_ignore_ascii_case(&saved.exe)
            && let Err(e) = claim(scheme, &exe)
        {
            log::warn!("could not point {scheme}: at spot's new path: {e:#}");
            return;
        }
    }
    let _ = config::save_protocol(&SavedProtocol { exe, ..saved });
}

/// Point one scheme at `exe`, writing the three values Windows reads.
fn claim(scheme: &str, exe: &str) -> Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu
        .create_subkey(format!(r"{CLASSES}\{scheme}"))
        .with_context(|| format!("could not claim {scheme}:"))?;
    key.set_value("", &format!("URL:{scheme} Protocol"))?;
    // Its presence is the whole of the signal; the value is always empty.
    key.set_value("URL Protocol", &"")?;
    let (command, _) = key.create_subkey(r"shell\open\command")?;
    command.set_value("", &format!("\"{exe}\" \"%1\""))?;
    Ok(())
}

fn open_command(hkcu: &RegKey, scheme: &str) -> Option<String> {
    hkcu.open_subkey_with_flags(format!(r"{CLASSES}\{scheme}\shell\open\command"), KEY_READ)
        .ok()?
        .get_value::<String, _>("")
        .ok()
        .filter(|cmd| !cmd.trim().is_empty())
}

fn exe_path() -> Result<String> {
    Ok(std::env::current_exe()
        .context("could not find spot's own path")?
        .to_string_lossy()
        .into_owned())
}

/// The executable out of a `shell\open\command` string, quotes stripped.
fn command_exe(command: &str) -> String {
    let command = command.trim();
    match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or_default().to_string(),
        None => command
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string(),
    }
}

fn is_this_exe(command: &str) -> bool {
    exe_path().is_ok_and(|exe| command_exe(command).eq_ignore_ascii_case(&exe))
}

/// The best name a command line gives for the app behind it: its file name,
/// without the `.exe`. Enough to tell somebody what they are replacing.
fn app_name(command: &str) -> String {
    let exe = command_exe(command);
    let file = exe.rsplit(['\\', '/']).next().unwrap_or(&exe);
    let name = file.strip_suffix(".exe").unwrap_or(file);
    if name.is_empty() {
        "another app".to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_executable_out_of_a_command() {
        assert_eq!(
            command_exe(r#""C:\Program Files\Spotify\Spotify.exe" --uri="%1""#),
            r"C:\Program Files\Spotify\Spotify.exe"
        );
        assert_eq!(command_exe(r"C:\bin\spot.exe %1"), r"C:\bin\spot.exe");
        assert_eq!(command_exe(""), "");
    }

    #[test]
    fn names_the_app_behind_a_command() {
        assert_eq!(
            app_name(r#""C:\Program Files\Spotify\Spotify.exe" --uri="%1""#),
            "Spotify"
        );
        assert_eq!(app_name(r"C:\bin\spot.exe %1"), "spot");
        assert_eq!(app_name(""), "another app");
    }

    #[test]
    fn a_windows_default_outranks_the_class_key() {
        let outranked = Registration {
            holder: Holder::Spot,
            consented: true,
            user_choice: Some("Spotify".to_string()),
        };
        // Registered, but outranked. Saying "links open in spot" here would be
        // a claim the user could not act on.
        assert!(!outranked.in_force());
        assert!(outranked.describe().contains("Spotify"));

        let ours = Registration {
            holder: Holder::Spot,
            consented: true,
            user_choice: None,
        };
        assert!(ours.in_force());
        assert!(ours.describe().contains("spot"));
    }

    #[test]
    fn a_scheme_taken_back_reads_as_taken_back() {
        let displaced = Registration {
            holder: Holder::Other("Spotify".to_string()),
            consented: true,
            user_choice: None,
        };
        assert!(!displaced.in_force());
    }
}
