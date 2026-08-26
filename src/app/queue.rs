//! The play order that spot owns.
//!
//! spot drives librespot's `Player` directly, so nothing outside this process
//! decides which track plays next. Position is truth: there is no sort key
//! and no display permutation — the row you see at index `i` is the track
//! that plays at position `i`. That is what makes the player screen correct
//! without a fetch: spot wrote the order, thus spot can show it.

use rand::seq::SliceRandom;

use crate::app::state::Track;

#[derive(Debug, Clone)]
pub struct Queue {
    /// Every track, in play order.
    tracks: Vec<Track>,
    /// The track that plays now, as an index into `tracks`.
    index: usize,
    /// What the deck's context row calls this queue ("My Mix", "Search
    /// results", an album name).
    name: String,
    /// The order before the shuffle, kept so that `s` can restore it.
    /// `None` when the queue is not shuffled.
    natural: Option<Vec<Track>>,
    /// More tracks are still on the way from the Web API. The queue plays
    /// what it has and grows as the pages land — see [`Self::extend`].
    pub loading: bool,
    /// Identity of the source this queue was filled from — the client's
    /// track-cache key (`"liked"`, `"playlist:<id>"`, …) — when it has one.
    /// A page fetch extends only a queue whose key it matches, and the
    /// playlist table marks the playing row by it. `None` for a queue with
    /// no re-fetchable source (search results, top tracks).
    pub source_key: Option<String>,
    /// Stamp from `AppState::queue_generation` at install time. A background
    /// fill compares its own stamp against the live queue's, so a queue
    /// replaced mid-fetch stops being written to.
    pub generation: u64,
}

impl Queue {
    pub fn new(tracks: Vec<Track>, start: usize, name: impl Into<String>) -> Self {
        let index = if tracks.is_empty() {
            0
        } else {
            start.min(tracks.len() - 1)
        };
        Self {
            tracks,
            index,
            name: name.into(),
            natural: None,
            loading: false,
            source_key: None,
            generation: 0,
        }
    }

    /// Append a page of tracks that arrived after play started. The playing
    /// index does not move: the new rows land after everything already here.
    pub fn extend(&mut self, page: Vec<Track>) {
        if let Some(natural) = self.natural.as_mut() {
            natural.extend(page.iter().cloned());
        }
        self.tracks.extend(page);
    }

    /// The playing track.
    pub fn current(&self) -> Option<&Track> {
        self.tracks.get(self.index)
    }

    /// Step forward. At the end, wrap to 0 — repeat-all, as spot has always
    /// pinned it.
    pub fn advance(&mut self) -> Option<&Track> {
        if self.tracks.is_empty() {
            return None;
        }
        self.index = (self.index + 1) % self.tracks.len();
        self.current()
    }

    /// Step back. At 0, stay at 0 — going back restarts the first track
    /// rather than wrapping to the end.
    pub fn back(&mut self) -> Option<&Track> {
        self.index = self.index.saturating_sub(1);
        self.current()
    }

    /// Select any row.
    pub fn jump(&mut self, i: usize) -> Option<&Track> {
        if i >= self.tracks.len() {
            return None;
        }
        self.index = i;
        self.current()
    }

    /// `a`: put a track directly after the playing one.
    pub fn insert_next(&mut self, track: Track) {
        let at = if self.tracks.is_empty() {
            0
        } else {
            self.index + 1
        };
        // Into the natural order too, after the playing track's position
        // there, so a track added under shuffle survives turning it off.
        let playing = self.current().map(|c| c.uri.clone());
        if let Some(natural) = self.natural.as_mut() {
            let pos = playing
                .and_then(|uri| natural.iter().position(|t| t.uri == uri))
                .map(|p| p + 1)
                .unwrap_or(natural.len());
            natural.insert(pos, track.clone());
        }
        self.tracks.insert(at.min(self.tracks.len()), track);
    }

    /// Shuffle on: keep the playing track where the ear is — it moves to row
    /// 0 and keeps playing — and shuffle everything else behind it. Shuffle
    /// off: restore the natural order and find the playing track in it. The
    /// playing track never changes, thus `s` never interrupts the audio.
    pub fn shuffle(&mut self, on: bool) {
        if on == self.natural.is_some() {
            return;
        }
        if on {
            self.natural = Some(self.tracks.clone());
            if self.tracks.is_empty() {
                return;
            }
            let playing = self.tracks.remove(self.index);
            self.tracks.shuffle(&mut rand::rng());
            self.tracks.insert(0, playing);
            self.index = 0;
        } else {
            let playing = self.current().map(|t| t.uri.clone());
            if let Some(natural) = self.natural.take() {
                self.tracks = natural;
            }
            // By URI rather than a remembered index: extend and insert_next
            // both grew `natural` since the shuffle. A duplicated track finds
            // its first copy, which mismarks a row but never the audio.
            self.index = playing
                .and_then(|uri| self.tracks.iter().position(|t| t.uri == uri))
                .unwrap_or(0);
        }
    }

    /// The rows the player screen draws — the play order itself.
    pub fn rows(&self) -> &[Track] {
        &self.tracks
    }

    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::Credit;

    fn track(name: &str) -> Track {
        Track {
            uri: format!("spotify:track:{name}"),
            name: name.into(),
            artists: "artist".into(),
            album: "album".into(),
            release_year: "2020".into(),
            duration_ms: 1000,
            track_number: 1,
            album_id: None,
            credits: vec![Credit {
                name: "artist".into(),
                id: None,
            }],
            cover_url: None,
        }
    }

    fn queue(names: &[&str], start: usize) -> Queue {
        Queue::new(names.iter().map(|n| track(n)).collect(), start, "Q")
    }

    #[test]
    fn advance_wraps_at_the_end() {
        let mut q = queue(&["a", "b", "c"], 1);
        assert_eq!(q.advance().unwrap().name, "c");
        assert_eq!(q.advance().unwrap().name, "a", "repeat-all wraps to 0");
        assert!(queue(&[], 0).advance().is_none());
    }

    #[test]
    fn back_stops_at_zero() {
        let mut q = queue(&["a", "b"], 1);
        assert_eq!(q.back().unwrap().name, "a");
        assert_eq!(q.back().unwrap().name, "a", "0 stays 0");
    }

    #[test]
    fn jump_selects_any_row_and_refuses_out_of_range() {
        let mut q = queue(&["a", "b", "c"], 0);
        assert_eq!(q.jump(2).unwrap().name, "c");
        assert_eq!(q.index(), 2);
        assert!(q.jump(3).is_none());
        assert_eq!(q.index(), 2, "a refused jump moves nothing");
    }

    #[test]
    fn insert_next_lands_directly_after_the_playing_row() {
        let mut q = queue(&["a", "b", "c"], 1);
        q.insert_next(track("x"));
        let names: Vec<&str> = q.rows().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "x", "c"]);
        assert_eq!(q.current().unwrap().name, "b", "the audio does not move");

        let mut empty = queue(&[], 0);
        empty.insert_next(track("x"));
        assert_eq!(empty.current().unwrap().name, "x");
    }

    #[test]
    fn shuffle_keeps_the_playing_track_at_row_zero() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 2);
        q.shuffle(true);
        assert_eq!(q.index(), 0);
        assert_eq!(q.current().unwrap().name, "c", "the audio does not move");
        assert_eq!(q.len(), 5);
    }

    #[test]
    fn unshuffle_restores_the_order_and_finds_the_playing_track() {
        let mut q = queue(&["a", "b", "c", "d", "e"], 2);
        q.shuffle(true);
        // Move along the shuffled order, then restore.
        q.advance();
        let playing = q.current().unwrap().name.clone();
        q.shuffle(false);
        let names: Vec<&str> = q.rows().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d", "e"]);
        assert_eq!(q.current().unwrap().name, playing);
    }

    #[test]
    fn a_track_added_under_shuffle_survives_turning_it_off() {
        let mut q = queue(&["a", "b", "c"], 0);
        q.shuffle(true);
        q.insert_next(track("x"));
        q.shuffle(false);
        assert!(q.rows().iter().any(|t| t.name == "x"));
        assert_eq!(q.len(), 4);
    }

    #[test]
    fn extend_does_not_move_the_index() {
        let mut q = queue(&["a", "b"], 1);
        q.extend(vec![track("c"), track("d")]);
        assert_eq!(q.index(), 1);
        assert_eq!(q.current().unwrap().name, "b");
        assert_eq!(q.len(), 4);
    }

    #[test]
    fn extend_under_shuffle_reaches_the_natural_order_too() {
        let mut q = queue(&["a", "b", "c"], 0);
        q.shuffle(true);
        q.extend(vec![track("d")]);
        assert_eq!(q.len(), 4);
        q.shuffle(false);
        assert_eq!(q.len(), 4, "the page vanished with the shuffle");
        assert!(q.rows().iter().any(|t| t.name == "d"));
    }
}
