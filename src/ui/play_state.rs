//! What the deck is doing, as one answer.
//!
//! Two things on screen report it: the status word pinned to the top-right of
//! the header ([`super::top_row`]) and the transport's play/pause pill
//! ([`super::deck`]). They used to work it out separately — the word off the
//! audio tap, the pill off a snapshot flag and a `pending_play` that is only
//! cleared when a three-second poll agrees — and they disagreed for as long as
//! those two clocks were apart. The pill sat on `⋯ load` for half a minute
//! under a corner already saying `STREAMING`, and a station still connecting
//! offered `■ pause` under a corner saying `LOADING`.
//!
//! Both read this instead, so the two can only ever say the same thing.

use std::time::Duration;

use crate::app::state::AppState;

/// How long the tap may go quiet before a source that says it is playing is
/// read as still loading. Longer than the visualizer's own freshness window,
/// which is tuned to drop the bars' colour the instant audio stops: a word
/// that flickers between `STREAMING` and `LOADING` on a momentary underrun is
/// worse than one that waits.
const LOAD_WITHIN: Duration = Duration::from_millis(1200);

/// The three states a deck can be in, as the screen has to draw them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// Sound is coming out. The corner says `STREAMING` or `RADIO` in the
    /// accent; the pill offers `■ pause`.
    Playing,
    /// Asked for and not heard yet — a track being fetched, a station still
    /// connecting. The corner says `LOADING`; the pill is not drawn at all,
    /// because neither word on it would be true and a click has nothing to
    /// toggle.
    Loading,
    /// Loaded and quiet. The corner says the source's name in grey; the pill
    /// offers `▶ play`.
    Paused,
}

/// What is making sound, and whether it is. `None` when nothing is — neither
/// a station nor a Spotify snapshot, so there is nothing to report on.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    /// `RADIO` or `STREAMING`: which source the answer is about.
    pub word: &'static str,
    pub state: PlayState,
    /// Whether the audio is ours to judge — see [`status`].
    pub ours: bool,
    /// Whether samples are arriving from the local sink right now. The pulsing
    /// dot rides this, so it is handed back rather than sampled twice.
    pub fresh: bool,
}

/// Work out what the deck is doing.
///
/// Radio is checked first, as it is everywhere else: the two sources are
/// mutually exclusive by construction, and while a station is on the Spotify
/// snapshot is kept only so stopping the stream puts the last track back.
pub fn status(state: &AppState) -> Option<Status> {
    // A play asked for and not heard yet. Its snapshot says it is not playing,
    // because it is not — nothing has come out of the sink. But it is on its
    // way, and that is what this is for: without the extra term the whole gap
    // read as a dim `STREAMING`, which is the opposite of the truth. It also
    // covers the ordinary track boundary, where librespot's `Stopped` clears
    // `is_playing` for the moment the next track takes to load.
    let switching = state.pending_play.is_some();
    let (word, claims) = match (&state.radio, &state.playback) {
        (Some(r), _) => ("RADIO", r.is_playing),
        (None, Some(pb)) => ("STREAMING", pb.is_playing || switching),
        (None, None) => return None,
    };

    // Whether the audio is ours to judge. Playing on a phone, librespot is
    // idle and the tap will never fill — reading that as "loading" would leave
    // the word stuck yellow for the length of the record, and take the pill
    // off a transport that still works. A play we asked for is ours by
    // definition, whatever the last poll was describing.
    let ours = switching
        || state.radio.is_some()
        || state.playback.as_ref().is_some_and(|pb| pb.is_local_device);
    let fresh = state.audio_tap.is_fresh(LOAD_WITHIN);

    // Claims to be playing, but nothing has come out of it yet: a station
    // still connecting and prefetching, or a track still being fetched. The
    // radio player clears the tap before it connects, so this window is
    // exactly the buffering one.
    //
    // Note what is *not* consulted: `pending_play` on its own. It stays armed
    // until a `/me/player` poll names the new track, which lags the audio by
    // seconds and sometimes the better part of a minute; the tap knows the
    // moment the first sample lands. Sound coming out is the end of loading,
    // whatever the poll still believes.
    let play = match (claims, ours && !fresh) {
        (true, true) => PlayState::Loading,
        (true, false) => PlayState::Playing,
        (false, _) => PlayState::Paused,
    };
    Some(Status {
        word,
        state: play,
        ours,
        fresh,
    })
}

/// The state to draw a transport in when there is nothing to report on. The
/// decks only draw one over a live source, so this is the unreachable arm
/// rather than a real state.
pub fn or_paused(status: Option<Status>) -> PlayState {
    status.map_or(PlayState::Paused, |s| s.state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{PendingPlay, PlaybackSnapshot, RadioPlayback, RepeatMode};

    fn streaming() -> AppState {
        let mut st = AppState::new();
        st.playback = Some(PlaybackSnapshot {
            is_playing: true,
            progress_ms: 0,
            duration_ms: 1000,
            track_uri: Some("spotify:track:new".into()),
            context_uri: None,
            artist_id: None,
            album_id: None,
            track_name: "Envejecer".into(),
            artists: "Erameld".into(),
            album: "Días Despejados".into(),
            release_year: "2020".into(),
            cover_url: None,
            shuffle: false,
            repeat: RepeatMode::Off,
            volume_percent: 70,
            device_name: "spot".into(),
            is_local_device: true,
            fetched_at: std::time::Instant::now(),
        });
        st.audio_tap.push(&[0.0; 2048], 1.0);
        st
    }

    fn radio() -> AppState {
        let mut st = AppState::new();
        st.radio = Some(RadioPlayback {
            station: super::super::tests::station("s1", "KEXP"),
            is_playing: true,
            started_at: std::time::Instant::now(),
            title: std::sync::Arc::new(parking_lot::Mutex::new(None)),
            volume_percent: 50,
            matched: Default::default(),
        });
        st.audio_tap.push(&[0.0; 2048], 1.0);
        st
    }

    fn state_of(st: &AppState) -> PlayState {
        status(st).expect("a source is playing").state
    }

    /// Audio arriving is the end of loading, whatever `/me/player` still
    /// believes. This is the bug the pill used to have on its own: it read
    /// `pending_play`, which stays armed until a three-second poll names the
    /// new track and is re-armed by every librespot event in the meantime, so
    /// it sat on `⋯ load` for half a minute at a time — under a corner that
    /// had said `STREAMING` since the first sample.
    #[test]
    fn audio_arriving_ends_the_wait_whatever_the_poll_says() {
        let mut st = streaming();
        st.pending_play = Some(PendingPlay {
            expect_uri: Some("spotify:track:new".into()),
            prev_uri: Some("spotify:track:old".into()),
            since: std::time::Instant::now(),
        });
        assert_eq!(state_of(&st), PlayState::Playing);

        // And before it arrives, the same wait is a load.
        st.audio_tap.clear();
        assert_eq!(state_of(&st), PlayState::Loading);
    }

    /// A station that has been asked for and has not connected yet. The
    /// transport used to take `is_playing` at face value here and offer
    /// `■ pause` over silence, under a corner already saying `LOADING`.
    #[test]
    fn a_station_still_connecting_is_loading_not_playing() {
        let st = radio();
        assert_eq!(state_of(&st), PlayState::Playing);
        st.audio_tap.clear();
        assert_eq!(state_of(&st), PlayState::Loading);
    }

    /// Stopped is stopped: the tap is empty either way, and only the claim
    /// tells the two apart. Nothing here may read as loading, or the pill
    /// would vanish off a deck that is simply paused.
    #[test]
    fn a_stopped_source_is_paused_not_loading() {
        let mut st = streaming();
        st.audio_tap.clear();
        st.playback.as_mut().unwrap().is_playing = false;
        assert_eq!(state_of(&st), PlayState::Paused);

        let mut st = radio();
        st.audio_tap.clear();
        st.radio.as_mut().unwrap().is_playing = false;
        assert_eq!(state_of(&st), PlayState::Paused);
    }

    /// Playing on a phone: librespot is idle and the tap will never fill, so
    /// judging it by our own audio would leave the deck stuck on `LOADING`
    /// with no pill for the length of the record.
    #[test]
    fn playback_on_another_device_is_never_loading() {
        let mut st = streaming();
        st.audio_tap.clear();
        st.playback.as_mut().unwrap().is_local_device = false;
        assert_eq!(state_of(&st), PlayState::Playing);
    }

    /// Nothing playing at all: no word for the corner, and the decks that
    /// would draw a transport are not drawn either.
    #[test]
    fn an_idle_app_reports_nothing() {
        assert!(status(&AppState::new()).is_none());
    }
}
