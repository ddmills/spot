//! Keeping spot current.
//!
//! spot is a download, not an install: there is no package manager behind it to
//! notice a new release, and nothing on screen names the version being run. So
//! the app asks GitHub itself, once a run, and can replace its own executable
//! with the answer.
//!
//! The release workflow publishes a bare `spot.exe` asset beside the zip on
//! every `v*` tag, which is what makes the swap a single download rather than
//! an unpack.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use semver::Version;
use serde::Deserialize;

const LATEST_RELEASE: &str = "https://api.github.com/repos/ddmills/spot/releases/latest";
/// The asset the release workflow attaches unpacked. The zip beside it holds
/// the same binary with the README and the licence, which an update does not
/// need.
const ASSET: &str = "spot.exe";

/// Where the incoming binary lands while it downloads, and where the running
/// one moves to make room for it.
const STAGED: &str = ".new";
const PREVIOUS: &str = ".old";

/// No overall timeout, unlike the client the rest of the app shares: this is
/// megabytes over whatever connection the user has, and a cap that suits a
/// cover-art fetch would abandon it halfway.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A published release worth moving to.
///
/// Only [`latest`] builds one, and it refuses to return a release that is not
/// newer than the running build — so holding one means an update is genuinely
/// available.
#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// The newest release, or `None` when this build is already it.
///
/// Takes the client the app already shares, which carries the `spot/<version>`
/// user agent GitHub insists on.
pub async fn latest(http: &reqwest::Client) -> Result<Option<Release>> {
    let release: GithubRelease = http
        .get(LATEST_RELEASE)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("could not reach GitHub")?
        .error_for_status()
        .context("GitHub refused the release request")?
        .json()
        .await
        .context("could not read GitHub's reply")?;
    newer_than_running(release)
}

fn newer_than_running(release: GithubRelease) -> Result<Option<Release>> {
    let GithubRelease { tag_name, assets } = release;
    let version = parse_tag(&tag_name)
        .with_context(|| format!("GitHub's latest release is tagged {tag_name:?}"))?;
    if version <= running() {
        return Ok(None);
    }
    let url = assets
        .into_iter()
        .find(|asset| asset.name == ASSET)
        .map(|asset| asset.browser_download_url)
        .with_context(|| format!("release {tag_name} has no {ASSET} to download"))?;
    Ok(Some(Release { tag: tag_name, url }))
}

/// Tags are the cargo version with a `v` in front — see the release workflow.
fn parse_tag(tag: &str) -> Result<Version> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    Version::parse(version).map_err(Into::into)
}

fn running() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate's own version is valid semver")
}

/// Download `release` and put it where this executable is.
pub async fn install(release: &Release) -> Result<()> {
    let exe = std::env::current_exe().context("could not find spot's own path")?;
    download_to(release, exe).await
}

/// The half of [`install`] that does not decide where the binary goes, so a
/// test can point it somewhere that is not the running program.
async fn download_to(release: &Release, exe: PathBuf) -> Result<()> {
    let http = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("spot/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("could not open a connection for the download")?;
    let bytes = http
        .get(&release.url)
        .send()
        .await
        .with_context(|| format!("could not download {}", release.tag))?
        .error_for_status()
        .with_context(|| format!("GitHub refused the download of {}", release.tag))?
        .bytes()
        .await
        .context("the download ended early")?;

    tokio::task::spawn_blocking(move || swap(&exe, &bytes))
        .await
        .context("the install did not finish")?
}

/// Put `bytes` at `exe`, keeping the file that is already there.
///
/// Windows will not delete or overwrite a running image but will happily rename
/// one, which is the whole reason for the two steps: the running spot moves
/// aside under [`PREVIOUS`] and the download takes its name. The displaced file
/// stays on disk until the next run clears it — see [`clean_previous`].
fn swap(exe: &Path, bytes: &[u8]) -> Result<()> {
    let staged = sibling(exe, STAGED);
    let previous = sibling(exe, PREVIOUS);

    fs::write(&staged, bytes).with_context(|| format!("could not write {}", staged.display()))?;
    let _ = fs::remove_file(&previous);
    if let Err(e) = fs::rename(exe, &previous) {
        let _ = fs::remove_file(&staged);
        return Err(e).with_context(|| format!("could not move {} aside", exe.display()));
    }
    if let Err(e) = fs::rename(&staged, exe) {
        let _ = fs::rename(&previous, exe);
        let _ = fs::remove_file(&staged);
        return Err(e).with_context(|| format!("could not put the new spot at {}", exe.display()));
    }
    Ok(())
}

/// Remove the executable a previous run replaced.
///
/// Best effort on purpose: a file that will not go is a few megabytes beside a
/// working spot, and there is nothing the user could do about a message saying
/// so.
pub fn clean_previous() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let _ = fs::remove_file(sibling(&exe, PREVIOUS));
}

/// `exe` with `suffix` appended, so `spot.exe` becomes `spot.exe.old` rather
/// than losing the extension the way `with_extension` would.
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(exe);
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, assets: &[&str]) -> GithubRelease {
        let assets: Vec<String> = assets
            .iter()
            .map(|name| {
                format!(
                    r#"{{"name":"{name}","browser_download_url":"https://example.test/{name}"}}"#
                )
            })
            .collect();
        serde_json::from_str(&format!(
            r#"{{"tag_name":"{tag}","assets":[{}]}}"#,
            assets.join(",")
        ))
        .expect("the fixture is valid release JSON")
    }

    #[test]
    fn a_newer_tag_yields_its_exe() {
        let found = newer_than_running(release("v99.0.0", &["spot.exe", "spot-v99.0.0.zip"]))
            .expect("a well-formed release parses")
            .expect("99.0.0 outranks any version spot will ship");
        assert_eq!(found.tag, "v99.0.0");
        assert_eq!(found.url, "https://example.test/spot.exe");
    }

    #[test]
    fn the_running_version_is_not_an_update() {
        let same = format!("v{}", env!("CARGO_PKG_VERSION"));
        assert!(
            newer_than_running(release(&same, &["spot.exe"]))
                .expect("a well-formed release parses")
                .is_none()
        );
        assert!(
            newer_than_running(release("v0.0.1", &["spot.exe"]))
                .expect("a well-formed release parses")
                .is_none()
        );
    }

    /// A prerelease sorts under the version it leads to, so an `-rc` tag must
    /// not offer itself over the release of the same number.
    #[test]
    fn a_prerelease_does_not_outrank_its_release() {
        let rc = format!("v{}-rc.1", env!("CARGO_PKG_VERSION"));
        assert!(
            newer_than_running(release(&rc, &["spot.exe"]))
                .expect("a prerelease tag parses")
                .is_none()
        );
    }

    #[test]
    fn a_release_without_the_exe_is_an_error() {
        assert!(newer_than_running(release("v99.0.0", &["spot-v99.0.0.zip"])).is_err());
    }

    #[test]
    fn an_unparseable_tag_is_an_error() {
        assert!(newer_than_running(release("nightly", &["spot.exe"])).is_err());
    }

    /// The swap leaves the new binary in place and the old one beside it,
    /// which is what makes the next start able to clear up.
    #[test]
    fn the_swap_keeps_the_replaced_executable() {
        let dir = std::env::temp_dir().join("spot-swap-keeps");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        let exe = dir.join("spot.exe");
        fs::write(&exe, b"old").expect("the standing executable");

        swap(&exe, b"new").expect("the swap should land");

        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert_eq!(fs::read(sibling(&exe, PREVIOUS)).unwrap(), b"old");
        assert!(!sibling(&exe, STAGED).exists(), "the staged copy should go");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A swap that cannot complete must leave the executable that was working
    /// exactly where it was.
    #[test]
    fn a_failed_swap_puts_the_old_executable_back() {
        let dir = std::env::temp_dir().join("spot-swap-restores");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("a scratch directory");
        let exe = dir.join("spot.exe");
        fs::write(&exe, b"old").expect("the standing executable");
        // A directory at the staged path: the download cannot be written and
        // the swap has to give up before it moves anything.
        fs::create_dir(sibling(&exe, STAGED)).expect("the obstruction");

        assert!(swap(&exe, b"new").is_err());
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_staged_and_previous_names_keep_the_extension() {
        let exe = Path::new("C:/tools/spot.exe");
        assert!(sibling(exe, STAGED).ends_with("spot.exe.new"));
        assert!(sibling(exe, PREVIOUS).ends_with("spot.exe.old"));
    }

    /// Asks the real API. Ignored by default — it needs a network, and its
    /// answer changes with every release.
    ///
    /// Run it with `cargo test update::tests::live -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_latest_release_resolves() {
        let http = reqwest::Client::builder()
            .user_agent(concat!("spot/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap();
        match latest(&http).await.expect("the release API should answer") {
            Some(release) => println!("update to {} from {}", release.tag, release.url),
            None => println!("v{} is the latest release", env!("CARGO_PKG_VERSION")),
        }
    }

    /// Downloads the published release over a stand-in and checks that what
    /// landed is a Windows executable. Everything an update does except being
    /// the running program.
    ///
    /// Ignored by default — it needs a network and pulls megabytes. Run it
    /// with `cargo test update::tests::live_install -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore]
    async fn live_install_replaces_the_target() {
        let http = reqwest::Client::builder()
            .user_agent(concat!("spot/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap();
        let published: GithubRelease = http
            .get(LATEST_RELEASE)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let url = published
            .assets
            .into_iter()
            .find(|asset| asset.name == ASSET)
            .expect("the release publishes spot.exe")
            .browser_download_url;
        let release = Release {
            tag: published.tag_name,
            url,
        };

        let dir = std::env::temp_dir().join("spot-live-install");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("spot.exe");
        fs::write(&exe, b"old").unwrap();

        download_to(&release, exe.clone())
            .await
            .expect("the published release should install");

        let landed = fs::read(&exe).unwrap();
        println!("{} wrote {} bytes", release.tag, landed.len());
        assert_eq!(&landed[..2], b"MZ", "what landed is not an executable");
        assert_eq!(fs::read(sibling(&exe, PREVIOUS)).unwrap(), b"old");
        let _ = fs::remove_dir_all(&dir);
    }
}
