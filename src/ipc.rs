//! Handing a link to the spot that is already running.
//!
//! A clicked link must not start a second player: two of them would fight over
//! the audio device and over the Spotify session. So a launch that carries a
//! link first offers it to a running copy, and only runs itself when there is
//! nobody to give it to.
//!
//! The offer doubles as the single-instance test, which is why there is no
//! mutex here — a pipe that answers *is* a running spot. A launch that carries
//! no link never asks: a second window someone opened on purpose is still
//! theirs to open.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::command::AppCommand;
use crate::link::{self, Link};

/// Marks the one line this protocol has, so a later spelling can be told from
/// this one rather than guessed at.
const PREFIX: &str = "v1 ";

/// Nothing this pipe carries comes near it. Anything running as the user may
/// write here, so the read stops long before a sender that never stops can
/// cost anything.
const MAX_LINE: usize = 2048;

/// How long the far end has to take the link and answer for it.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);

/// A single byte back, so the sender knows the link arrived rather than
/// assuming it from a write a dying process would also accept.
const ACK: u8 = b'.';

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

/// One pipe per user, so two people signed in to one machine keep their own.
fn pipe_name() -> String {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(32)
        .collect();
    if user.is_empty() {
        r"\\.\pipe\spot".to_string()
    } else {
        format!(r"\\.\pipe\spot-{user}")
    }
}

/// Offer `target` to a running spot. `true` means it took it and this process
/// has nothing left to do.
///
/// False on every other outcome, including a spot that is on its way out: the
/// caller then runs normally, which opens the link in a window of its own. Two
/// players is a worse answer than one, but both are better than dropping a
/// link the user clicked.
pub async fn forward(target: &Link) -> bool {
    forward_on(&pipe_name(), target).await
}

async fn forward_on(name: &str, target: &Link) -> bool {
    let Some(mut client) = connect(name).await else {
        return false;
    };
    let line = format!("{PREFIX}{}\n", target.to_uri());

    let handoff = async {
        client.write_all(line.as_bytes()).await.ok()?;
        client.flush().await.ok()?;
        let mut ack = [0u8; 1];
        client.read_exact(&mut ack).await.ok()?;
        (ack[0] == ACK).then_some(())
    };
    match tokio::time::timeout(HANDOFF_TIMEOUT, handoff).await {
        Ok(Some(())) => true,
        Ok(None) => {
            log::info!("the running spot did not take the link");
            false
        }
        Err(_) => {
            log::info!("the running spot did not answer in {HANDOFF_TIMEOUT:?}");
            false
        }
    }
}

/// Open the pipe, waiting out a server busy with another connection.
///
/// Busy is normal rather than a failure: the listener serves one caller at a
/// time, and two links clicked together arrive within microseconds of each
/// other.
async fn connect(name: &str) -> Option<NamedPipeClient> {
    const ATTEMPTS: u8 = 10;
    const BUSY_WAIT: Duration = Duration::from_millis(50);

    for _ in 0..ATTEMPTS {
        match ClientOptions::new().open(name) {
            Ok(client) => return Some(client),
            // Nobody is listening, which is the ordinary case: this is the
            // only spot there is.
            Err(e) if e.raw_os_error() == Some(ERROR_FILE_NOT_FOUND) => return None,
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                tokio::time::sleep(BUSY_WAIT).await;
            }
            Err(e) => {
                log::warn!("could not reach the running spot: {e}");
                return None;
            }
        }
    }
    log::warn!("the running spot stayed busy");
    None
}

/// Take links from later launches for the lifetime of this process.
///
/// Best effort, and silent when it cannot start. Losing the listener costs a
/// clicked link its shortcut — the second process runs itself instead — and
/// that is not worth refusing to start spot over.
pub fn listen(tx: UnboundedSender<AppCommand>) {
    listen_on(pipe_name(), tx);
}

fn listen_on(name: String, tx: UnboundedSender<AppCommand>) {
    // The one guard against two listeners: a genuine startup race has one
    // loser, and the loser simply runs as an ordinary second instance.
    let first = match ServerOptions::new().first_pipe_instance(true).create(&name) {
        Ok(server) => server,
        Err(e) => {
            log::info!("not taking links on {name}: {e}");
            return;
        }
    };
    tokio::spawn(async move {
        let mut server = first;
        loop {
            if let Err(e) = server.connect().await {
                log::warn!("the link pipe stopped: {e}");
                return;
            }
            // The next instance is claimed before this one is served, so a
            // link arriving while the last is being read is not refused.
            let serving = server;
            server = match ServerOptions::new().create(&name) {
                Ok(next) => next,
                Err(e) => {
                    log::warn!("the link pipe could not be reopened: {e}");
                    return;
                }
            };
            let tx = tx.clone();
            tokio::spawn(serve(serving, tx));
        }
    });
}

/// Read one line and act on it, if it says anything spot can open.
///
/// Everything arriving here is treated as hostile: any process running as this
/// user may write to the pipe, so the read is capped and the link goes through
/// the same parser the search prompt uses.
async fn serve(mut pipe: NamedPipeServer, tx: UnboundedSender<AppCommand>) {
    let mut buf = Vec::new();
    let read = async {
        let mut byte = [0u8; 1];
        while buf.len() < MAX_LINE {
            match pipe.read(&mut byte).await {
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => buf.push(byte[0]),
                Err(_) => return false,
            }
        }
        true
    };
    if !matches!(tokio::time::timeout(HANDOFF_TIMEOUT, read).await, Ok(true)) {
        return;
    }

    let Ok(line) = String::from_utf8(buf) else {
        return;
    };
    let Some(uri) = line.trim_end_matches('\r').strip_prefix(PREFIX) else {
        log::warn!("ignoring an unreadable line on the link pipe");
        return;
    };
    let Ok(target) = link::parse(uri) else {
        log::warn!("ignoring an unopenable link on the link pipe");
        return;
    };
    // Only once the link is known good, so a sender is never told spot took
    // something it threw away.
    let _ = pipe.write_all(&[ACK]).await;
    let _ = pipe.flush().await;
    let _ = tx.send(AppCommand::OpenLink(target));
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    const ID: &str = "4uLU6hMCjMI75M1A2tKUQC";

    /// A pipe of this test's own. The real name is one per user, which every
    /// test — and any spot running on the same machine — would otherwise
    /// share.
    fn test_pipe(tag: &str) -> String {
        format!(r"\\.\pipe\spot-test-{tag}")
    }

    /// What the far end saw, given as long as a slow machine needs and no
    /// longer. Nothing here waits on a network.
    async fn next_command(rx: &mut mpsc::UnboundedReceiver<AppCommand>) -> Option<AppCommand> {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    async fn a_link_reaches_the_running_spot() {
        let name = test_pipe("handoff");
        let (tx, mut rx) = mpsc::unbounded_channel();
        listen_on(name.clone(), tx);

        assert!(forward_on(&name, &Link::Album(ID.into())).await);
        let Some(AppCommand::OpenLink(target)) = next_command(&mut rx).await else {
            panic!("the link did not arrive")
        };
        assert_eq!(target, Link::Album(ID.into()));
    }

    /// One listener serves any number of launches, not just the first.
    #[tokio::test]
    async fn the_listener_keeps_taking_links() {
        let name = test_pipe("repeat");
        let (tx, mut rx) = mpsc::unbounded_channel();
        listen_on(name.clone(), tx);

        for expected in [Link::Track(ID.into()), Link::Artist(ID.into())] {
            assert!(forward_on(&name, &expected).await);
            let Some(AppCommand::OpenLink(target)) = next_command(&mut rx).await else {
                panic!("{expected:?} did not arrive")
            };
            assert_eq!(target, expected);
        }
    }

    /// The ordinary case: this is the only spot there is, so the launch runs
    /// itself rather than waiting on a pipe nobody holds.
    #[tokio::test]
    async fn nobody_listening_is_not_an_error() {
        assert!(!forward_on(&test_pipe("empty"), &Link::Track(ID.into())).await);
    }

    /// Anything running as this user can write here, so nothing that is not a
    /// link spot can open may reach the command channel.
    #[tokio::test]
    async fn junk_on_the_pipe_reaches_nothing() {
        let name = test_pipe("junk");
        let (tx, mut rx) = mpsc::unbounded_channel();
        listen_on(name.clone(), tx);

        let overlong = "x".repeat(MAX_LINE * 2);
        for line in [
            // No version marker.
            format!("spotify:album:{ID}\n"),
            // Marked, but not a link.
            format!("{PREFIX}rm -rf /\n"),
            // Marked, and a link to something spot does not play.
            format!("{PREFIX}spotify:episode:{ID}\n"),
            // Longer than the read will ever take.
            format!("{PREFIX}{overlong}\n"),
        ] {
            let mut client = connect(&name).await.expect("the listener is up");
            let _ = client.write_all(line.as_bytes()).await;
            let _ = client.flush().await;
            // No ack is coming, so the far end is given a moment to prove it
            // did nothing rather than merely being slow.
            let mut ack = [0u8; 1];
            let answered =
                tokio::time::timeout(Duration::from_millis(250), client.read_exact(&mut ack)).await;
            assert!(
                answered.is_err() || answered.unwrap().is_err(),
                "acked {line:?}"
            );
        }

        // A good link still lands afterwards: none of the above wedged the
        // listener.
        assert!(forward_on(&name, &Link::Playlist(ID.into())).await);
        assert!(matches!(
            next_command(&mut rx).await,
            Some(AppCommand::OpenLink(Link::Playlist(_)))
        ));
        // And nothing else arrived alongside it.
        assert!(rx.try_recv().is_err());
    }

    /// Two people signed in to one machine keep their own.
    #[test]
    fn the_pipe_is_named_per_user() {
        let name = pipe_name();
        let local = r"\\.\pipe\";
        assert!(name.starts_with(local), "{name}");
        // A separator inside the name would end it early and point somewhere
        // else entirely.
        assert!(!name[local.len()..].contains('\\'), "{name}");
    }
}
