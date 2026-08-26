//! What the deck is doing, as one answer.
//!
//! Two things on screen report it: the status word pinned to the top-right of
//! the header ([`super::top_row`]) and the transport's play/pause pill
//! ([`super::deck`]). Both read this one answer, so the two can only ever say
//! the same thing. Deriving it separately — the word off the audio tap, the
//! pill off a snapshot flag and a three-second poll — lets them disagree for
//! as long as those two clocks are apart.

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
/// a station nor a Spotify transport, so there is nothing to report on.
#[derive(Debug, Clone, Copy)]
pub struct Status {
    /// `RADIO` or `STREAMING`: which source the answer is about.
    pub word: &'static str,
    pub state: PlayState,
    /// Whether samples are arriving from the local sink right now. The pulsing
    /// dot rides this, so it is handed back rather than sampled twice.
    pub fresh: bool,
}

/// Work out what the deck is doing.
///
/// Radio is checked first, as it is everywhere else: the two sources are
/// mutually exclusive by construction, and while a station is on the Spotify
/// queue is kept only so stopping the stream puts the last track back.
///
/// The audio is always ours to judge — spot is the only player, and every
/// sample it makes goes through the tap.
pub fn status(state: &AppState) -> Option<Status> {
    let (word, claims) = match (&state.radio, &state.playback) {
        (Some(r), _) => ("RADIO", r.is_playing),
        (None, Some(pb)) => ("STREAMING", pb.is_playing),
        (None, None) => return None,
    };

    let fresh = state.audio_tap.is_fresh(LOAD_WITHIN);

    // Claims to be playing, but nothing has come out of it yet: a station
    // still connecting and prefetching, or a track still being fetched. Both
    // paths clear the tap before they start, so this window is exactly the
    // buffering one — and sound arriving is the end of it.
    let play = match (claims, fresh) {
        (true, false) => PlayState::Loading,
        (true, true) => PlayState::Playing,
        (false, _) => PlayState::Paused,
    };
    Some(Status {
        word,
        state: play,
        fresh,
    })
}

/// The state to draw a transport in when there is nothing to report on. The
/// decks only draw one over a live source, so this is the unreachable arm
/// rather than a real state.
pub fn or_paused(status: Option<Status>) -> PlayState {
    status.map_or(PlayState::Paused, |s| s.state)
}

/// Where the radio deck's outer transport controls lead.
///
/// Computed here for the reason [`status`] is: the bottom bar and the player
/// view both draw the row, and a control that is offered on one and not the
/// other is a control that moves when you press `v`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RadioSteps {
    /// Something was playing before this station.
    pub back: bool,
    pub forward: RadioForward,
}

/// What the right-hand control of the radio transport offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RadioForward {
    /// Nothing ahead and no country to walk: the control is not drawn.
    #[default]
    None,
    /// A station `◂◂ previous` stepped out of.
    Next,
    /// No station stepped out of, so the control moves down the playing
    /// station's own country instead.
    Seek,
}

/// What the radio transport row can offer this frame.
///
/// `Seek` needs a country because the directory is asked by ISO code — a
/// station that names none has nothing to walk through.
pub fn radio_steps(state: &AppState) -> RadioSteps {
    let has_country = state
        .radio
        .as_ref()
        .is_some_and(|r| !r.station.countrycode.is_empty());
    let forward = if !state.listen_forward.is_empty() {
        RadioForward::Next
    } else if has_country {
        RadioForward::Seek
    } else {
        RadioForward::None
    };
    RadioSteps {
        back: !state.listen_back.is_empty(),
        forward,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{Playback, RadioPlayback};

    fn streaming() -> AppState {
        let mut st = AppState::new();
        st.playback = Some(Playback::started(70, false));
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
            channels: Default::default(),
            volume_percent: 50,
            matched: Default::default(),
            failure: None,
            seek_attempt: 0,
            tune_seq: 0,
        });
        st.audio_tap.push(&[0.0; 2048], 1.0);
        st
    }

    fn state_of(st: &AppState) -> PlayState {
        status(st).expect("a source is playing").state
    }

    /// Audio arriving is the end of loading, and its absence under a playing
    /// claim is the whole of it: a track asked for and not yet audible reads
    /// as `LOADING`, and turns to `STREAMING` on the first sample.
    #[test]
    fn audio_arriving_ends_the_wait() {
        let st = streaming();
        assert_eq!(state_of(&st), PlayState::Playing);
        st.audio_tap.clear();
        assert_eq!(state_of(&st), PlayState::Loading);
    }

    /// A station that has been asked for and has not connected yet. Taking
    /// `is_playing` at face value here offers `■ pause` over silence, under a
    /// corner already saying `LOADING`.
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

    /// Nothing playing at all: no word for the corner, and the decks that
    /// would draw a transport are not drawn either.
    #[test]
    fn an_idle_app_reports_nothing() {
        assert!(status(&AppState::new()).is_none());
    }

    /// `next ▸▸` and `seek ▸▸` are one control with two meanings, and the
    /// forward path is what decides which: a station stepped out of is a
    /// station to go back to, and with none the control walks the country.
    #[test]
    fn the_right_hand_control_seeks_with_nothing_to_step_forward_to() {
        let mut st = radio();
        assert_eq!(radio_steps(&st).forward, RadioForward::Seek);
        st.listen_forward.push(crate::app::state::Listened::Spotify);
        assert_eq!(radio_steps(&st).forward, RadioForward::Next);
    }

    /// The directory is asked by ISO code, so a station that names no country
    /// has nothing to walk through — the same rule the deck's country link is
    /// drawn by.
    #[test]
    fn a_station_from_nowhere_offers_no_seek() {
        let mut st = radio();
        st.radio.as_mut().unwrap().station.countrycode.clear();
        assert_eq!(radio_steps(&st).forward, RadioForward::None);
    }

    /// A station that would not play reads as paused, not as loading, so the
    /// pill offers `▶ play` — which over a failed station means try again. A
    /// deck with no pill is a deck you cannot retry from.
    #[test]
    fn a_failed_station_reads_as_paused_not_loading() {
        let mut st = radio();
        st.audio_tap.clear();
        let r = st.radio.as_mut().unwrap();
        r.is_playing = false;
        r.failure = Some("could not reach the station".into());
        assert_eq!(state_of(&st), PlayState::Paused);
    }

    /// Back appears only with somewhere to go.
    #[test]
    fn the_back_control_waits_for_something_behind_it() {
        let mut st = radio();
        assert!(!radio_steps(&st).back);
        st.listen_back.push(crate::app::state::Listened::Spotify);
        assert!(radio_steps(&st).back);
    }
}
