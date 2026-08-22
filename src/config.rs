use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const DEVICE_NAME: &str = "spot";

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
