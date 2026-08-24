use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine config directory")?
        .join("spot");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn cache_dir() -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .context("could not determine cache directory")?
        .join("spot");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn log_file() -> Result<PathBuf> {
    Ok(cache_dir()?.join("spot.log"))
}

/// Persisted OAuth state. The refresh token is long-lived; access tokens are
/// re-derived from it on every run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedAuth {
    /// Client ID the refresh token belongs to; tokens from other IDs are
    /// unusable and get discarded.
    #[serde(default)]
    pub client_id: String,
    pub refresh_token: String,
}

fn auth_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("auth.json"))
}

pub fn load_auth() -> Option<SavedAuth> {
    let path = auth_file().ok()?;
    let data = fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

pub fn save_auth(auth: &SavedAuth) -> Result<()> {
    let path = auth_file()?;
    fs::write(&path, serde_json::to_string_pretty(auth)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn clear_auth() {
    if let Ok(path) = auth_file() {
        let _ = fs::remove_file(path);
    }
}

/// The stations you kept.
///
/// Its own file rather than a field of [`SavedAuth`]: one holds a credential
/// and is rewritten whenever Spotify hands out a new refresh token, the other
/// is a library and is rewritten when you star something. A radio directory has
/// no accounts to keep this in, so keeping it is spot's job.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedRadio {
    #[serde(default)]
    pub stations: Vec<crate::app::state::Station>,
}

fn radio_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("radio.json"))
}

/// Read the saved stations, or an empty list.
///
/// A missing file is the ordinary first-run case and a corrupt one is not worth
/// stopping for — the list rebuilds itself the moment you star a station.
pub fn load_radio() -> Vec<crate::app::state::Station> {
    let Ok(path) = radio_file() else {
        return Vec::new();
    };
    let Ok(data) = fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str::<SavedRadio>(&data)
        .map(|saved| saved.stations)
        .unwrap_or_default()
}

pub fn save_radio(stations: &[crate::app::state::Station]) -> Result<()> {
    let path = radio_file()?;
    let saved = SavedRadio {
        stations: stations.to_vec(),
    };
    fs::write(&path, serde_json::to_string_pretty(&saved)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// What each playlist holds, kept between runs.
///
/// In [`cache_dir`] rather than [`config_dir`]: this rebuilds itself from
/// Spotify, where `radio.json` beside `auth.json` is a library that cannot.
/// Written unindented — nothing reads it by eye, and the indentation would be
/// most of the file.
fn playlist_tracks_file() -> Result<PathBuf> {
    Ok(cache_dir()?.join("playlists.json"))
}

/// Read the cached playlist contents, or nothing.
///
/// A missing file is the ordinary first-run case and a corrupt one is not
/// worth stopping for — the prefetch fills it again either way.
pub fn load_playlist_tracks() -> HashMap<String, crate::app::state::PlaylistContents> {
    let Ok(path) = playlist_tracks_file() else {
        return HashMap::new();
    };
    let Ok(data) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn save_playlist_tracks(
    cache: &HashMap<String, crate::app::state::PlaylistContents>,
) -> Result<()> {
    let path = playlist_tracks_file()?;
    fs::write(&path, serde_json::to_string(cache)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::PlaylistContents;

    fn holding(snapshot: &str, ids: &[&str]) -> PlaylistContents {
        PlaylistContents {
            snapshot_id: snapshot.into(),
            track_ids: ids.iter().map(|id| (*id).to_string()).collect(),
        }
    }

    /// The file is the only thing that makes a second run instant, so what
    /// goes into it has to come back out.
    #[test]
    fn the_playlist_cache_round_trips() {
        let mut cache = HashMap::new();
        cache.insert("p1".to_string(), holding("s1", &["a", "b"]));
        cache.insert("p2".to_string(), holding("s2", &[]));
        let json = serde_json::to_string(&cache).unwrap();
        let back: HashMap<String, PlaylistContents> = serde_json::from_str(&json).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back["p1"].snapshot_id, "s1");
        assert_eq!(back["p1"].track_ids.len(), 2);
        assert!(back["p1"].track_ids.contains("a"));
        assert!(back["p2"].track_ids.is_empty());
    }

    /// A file from an older spot, or half-written by a kill, reads as nothing
    /// rather than stopping the run — the prefetch fills it again.
    #[test]
    fn a_corrupt_playlist_cache_reads_as_empty() {
        let empty: HashMap<String, PlaylistContents> = serde_json::from_str("{ nonsense")
            .ok()
            .unwrap_or_default();
        assert!(empty.is_empty());
    }
}
