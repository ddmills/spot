//! Scrolling loudness history, for the waveform mode.
//!
//! What an audio editor draws: one vertical bar per slice of time, as tall as
//! the signal was loud over that slice, standing either side of a centerline.
//! New bars arrive at the right edge and the picture walks left, so the field
//! is the last half-minute of the record rather than this instant of it — the
//! shape of a chorus is visible as a shape.
//!
//! A column is the energy of everything it covers, not the loudest moment in
//! it.
//!
//! Peak is the obvious measure and it is the wrong one here, because almost
//! everything spot plays has been through a limiter. On broadcast speech every
//! voiced sample sits on the ceiling, so peak reads the same for every column a
//! voice is in and the picture is a rectangle with gaps in it. Holding the
//! loudest *window* instead is no better: any column that catches a syllable's
//! onset takes the onset's level, and since a column is shorter than a word,
//! nearly all of them do.
//!
//! Integrating over the whole column is what gives a syllable a shape. A column
//! half filled with a word and half with the pause after it reads between the
//! two, which is exactly the texture a waveform is read by.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::audio_tap::AudioTap;

/// How long one column of the field stands for.
///
/// Deliberately slower than the draw loop. The loop redraws on input events as
/// well as on the frame tick, so scrolling per draw would run the picture
/// faster whenever the user touched a key; and at the tick's own 50 ms a field
/// holds only a couple of seconds, which is too little to read a section of a
/// song out of.
///
/// A column also has to be short against the beat. At a few hundred
/// milliseconds every column of a four-on-the-floor track catches a kick and
/// the picture is a slab; this leaves four or five columns to a bar at the
/// tempo most records are played at, so the ones between the hits are visibly
/// shorter.
const COLUMN_MS: u128 = 120;

/// Samples read per frame, ~46 ms at 44.1 kHz — a shade under the fast tick's
/// 50 ms, so successive reads very nearly abut. Reading a longer window would
/// smear each hit across the columns after it.
const READ: usize = 2048;

/// Reference attack and release, in seconds.
///
/// Both are long by the standards of the other analyzers, and for a reason
/// particular to this one: the whole point of the picture is that a loud
/// passage is *taller* than a quiet one. A reference that follows the music
/// closely normalizes that difference away and leaves a flat ribbon.
///
/// The attack is the knob that decides whether the picture has any texture.
/// Under a second and the reference tracks the syllables of a voice or the
/// bars of a track, every column reads level with it, and the field is a
/// rectangle. This is longer than a phrase and longer than a bar, so it settles
/// on what the passage is doing and lets the moments inside it differ — while
/// still converging within the first few seconds of a track.
const REF_ATTACK: f32 = 2.0;
const REF_RELEASE: f32 = 12.0;
/// Floor on the reference, so silence cannot wind the gain up onto the noise
/// under it.
const REF_MIN: f32 = 2e-3;

/// How far above the reference reads as full height.
///
/// Without it the reference *is* the ceiling, so anything at the passage's own
/// level pins at full and only the quiet moments have anywhere to go — half a
/// picture. This leaves the average column a little over half the field's
/// height, with room above it for the loud ones.
const HEADROOM_DB: f32 = 6.0;
/// Total display window, in dB, ending [`HEADROOM_DB`] above the reference. A
/// column below the bottom of it reads as silence.
const RANGE_DB: f32 = 20.0;
/// Expands the bottom of that window, on the same reasoning the spectrum's
/// contrast expander exists for: measured straight, the columns of a record
/// bunch into the top of their travel and the picture reads as a slab with a
/// ragged edge rather than as bars.
const CONTRAST: f32 = 1.6;
/// Below this a column is treated as true silence, so a muted stream rests on
/// the centerline instead of having the reference wind its noise floor up.
const SILENCE: f32 = 1e-4;

/// One column of the history, quantized. A byte is plenty: the field is at
/// most a few dozen rows and a column is drawn at half-cell resolution.
type Level = u8;

/// The scrolling envelope: one loudness per column, oldest first.
#[derive(Default)]
pub struct Wave {
    /// Closed columns, oldest first.
    columns: VecDeque<Level>,
    /// Signal energy accumulated into the open column and the number of samples
    /// it covers, so the column's level is the energy over the whole of it.
    /// `f64` because this sums the squares of tens of thousands of samples and
    /// `f32` visibly loses the quiet ones off the end of the mantissa.
    energy: f64,
    counted: usize,
    /// When the open column opened.
    opened: Option<Instant>,
    /// Running level the columns are measured against.
    reference: f32,
    samples: Vec<f32>,
    last_update: Option<Instant>,
}

impl Wave {
    /// Read the tap and fold this frame into the open column, closing it once
    /// it has stood for [`COLUMN_MS`]. `columns` is the field width.
    ///
    /// A stale tap contributes silence rather than nothing, so a paused player
    /// scrolls flat from the right instead of freezing mid-picture.
    pub fn update(&mut self, tap: &AudioTap, columns: usize, fresh: bool, now: Instant) {
        // The same clamp the other analyzers use: the first frame has no
        // reference point, and a long stall must not advance the reference by
        // a whole period in one step.
        let dt = match self.last_update {
            Some(prev) => now.saturating_duration_since(prev).as_secs_f32().min(0.5),
            None => 0.05,
        };
        self.last_update = Some(now);

        if fresh {
            tap.latest(&mut self.samples, READ);
        } else {
            self.samples.clear();
        }
        // A barely-filled ring is the first frames of a track, not silence, so
        // it moves neither the reference nor the column.
        let frame = if fresh && self.samples.len() >= 256 {
            let energy: f64 = self.samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
            let count = self.samples.len();
            self.advance_reference((energy / count as f64).sqrt() as f32, dt);
            (energy, count)
        } else if fresh {
            (0.0, 0)
        } else {
            // A stale tap really is silence, and has to weigh as much as a
            // frame of audio would — otherwise a paused player's open column
            // keeps whatever level it had when the audio stopped.
            (0.0, READ)
        };
        self.energy += frame.0;
        self.counted += frame.1;
        self.close(columns, now, frame);
    }

    /// Level of the column still being accumulated, in 0..=1.
    fn building(&self) -> f32 {
        match self.counted {
            0 => 0.0,
            counted => self.measure((self.energy / counted as f64).sqrt() as f32),
        }
    }

    /// The level at `column` in 0..=1, oldest column leftmost.
    ///
    /// The rightmost column is the one still being accumulated, so the field's
    /// leading edge shows what is playing now rather than what was playing up
    /// to [`COLUMN_MS`] ago. Columns the history has not reached yet read as
    /// silence, so a freshly-opened field scrolls in from the right instead of
    /// starting full.
    pub fn level(&self, column: usize, columns: usize) -> f32 {
        if column + 1 >= columns {
            return self.building();
        }
        let closed = match (column + self.columns.len()).checked_sub(closed_slots(columns)) {
            Some(i) => self.columns.get(i).copied().unwrap_or(0),
            None => 0,
        };
        closed as f32 / Level::MAX as f32
    }

    /// Close as many whole columns as have elapsed.
    ///
    /// Whole columns' worth of time, rather than "has one gone by": at the slow
    /// tick a frame is longer than a column, and closing one per frame would
    /// make the scroll rate the frame rate.
    fn close(&mut self, columns: usize, now: Instant, frame: (f64, usize)) {
        let opened = *self.opened.get_or_insert(now);
        let elapsed = now.saturating_duration_since(opened).as_millis();
        let closing = (elapsed / COLUMN_MS) as usize;
        if closing == 0 {
            return;
        }
        self.opened = Some(opened + Duration::from_millis((closing as u128 * COLUMN_MS) as u64));

        // The open column accumulated every frame of the gap, so it stands for
        // the whole of it. Capped at the field: a long stall has nothing to say
        // that more than one screenful of columns could show.
        //
        // What replaces it opens on this frame's own energy rather than on
        // nothing — it is the field's live edge, and starting it empty would
        // blank that edge for one frame every time a column closes.
        let closed = quantize(self.building());
        self.energy = frame.0;
        self.counted = frame.1;
        for _ in 0..closing.min(columns).max(1) {
            self.columns.push_back(closed);
        }
        while self.columns.len() > closed_slots(columns) {
            self.columns.pop_front();
        }
    }

    /// This window's level as a fraction of the field's half-height, measured
    /// against the running reference.
    ///
    /// In dB rather than on the raw amplitude because loudness is perceived
    /// that way: measured linearly, everything between a kick and the bed
    /// under it lands in the top of the travel and the bars stop differing
    /// from each other.
    fn measure(&self, rms: f32) -> f32 {
        if rms <= SILENCE {
            return 0.0;
        }
        let floor = 20.0 * self.reference.log10() + HEADROOM_DB - RANGE_DB;
        ((20.0 * rms.log10() - floor) / RANGE_DB)
            .clamp(0.0, 1.0)
            .powf(CONTRAST)
    }

    fn advance_reference(&mut self, rms: f32, dt: f32) {
        let tau = if rms > self.reference {
            REF_ATTACK
        } else {
            REF_RELEASE
        };
        self.reference += (rms - self.reference) * (1.0 - (-dt / tau).exp());
        self.reference = self.reference.max(REF_MIN);
    }
}

/// Columns of the field the closed history gets: all but the live one.
fn closed_slots(columns: usize) -> usize {
    columns.saturating_sub(1).max(1)
}

fn quantize(level: f32) -> Level {
    (level.clamp(0.0, 1.0) * Level::MAX as f32).round() as Level
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_tap::AudioTap;
    use crate::viz::SAMPLE_RATE;

    fn tap_of(signal: &[f32]) -> AudioTap {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = signal.iter().flat_map(|&s| [s as f64, s as f64]).collect();
        tap.push(&stereo, 1.0);
        tap
    }

    fn tone(amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / SAMPLE_RATE).sin())
            .collect()
    }

    /// Run `wave` over a static tap for `secs` of simulated time at the fast
    /// tick's rate, starting at `from`, and return where it ended.
    fn feed(wave: &mut Wave, tap: &AudioTap, columns: usize, from: Instant, secs: u64) -> Instant {
        for frame in 1..=secs * 20 {
            wave.update(tap, columns, true, from + Duration::from_millis(50 * frame));
        }
        from + Duration::from_millis(50 * secs * 20)
    }

    fn settled(signal: &[f32], columns: usize) -> Wave {
        let mut wave = Wave::default();
        feed(&mut wave, &tap_of(signal), columns, Instant::now(), 30);
        wave
    }

    /// Columns close on the clock, not on the frame — the draw loop redraws on
    /// input events too, so a per-frame history would scroll faster whenever
    /// the user touched a key.
    #[test]
    fn columns_close_on_elapsed_time_not_frame_count() {
        let tap = tap_of(&tone(0.5, READ));
        let count = |step_ms: u64, frames: u64| {
            let mut wave = Wave::default();
            let t0 = Instant::now();
            for frame in 0..frames {
                wave.update(&tap, 64, true, t0 + Duration::from_millis(step_ms * frame));
            }
            wave.columns.len()
        };
        // Four seconds either way: 80 frames at 50 ms or 16 at 250 ms.
        assert_eq!(count(50, 81), count(250, 17));
    }

    /// A column is the energy of everything it covered, so one that catches
    /// half a word and half the pause after it reads between the two. That
    /// in-between is what gives a syllable a shape — keeping the loudest window
    /// instead makes every column that touched the word read the same, which is
    /// most of them, and the field goes back to being a rectangle.
    #[test]
    fn a_column_integrates_what_it_covered() {
        let (loud, quiet) = (tap_of(&tone(0.6, READ)), tap_of(&tone(0.15, READ)));
        let mut wave = Wave::default();
        // Settle the reference on the loud signal, so all three readings below
        // are measured against the same level.
        let t = feed(&mut wave, &loud, 8, Instant::now(), 30);

        // Open a column at a known time so the frames land inside it, whatever
        // the settling above left the clock at.
        let run = |wave: &mut Wave, taps: [&AudioTap; 2]| {
            wave.opened = Some(t);
            wave.energy = 0.0;
            wave.counted = 0;
            for (i, tap) in taps.iter().enumerate() {
                wave.update(tap, 8, true, t + Duration::from_millis(1 + 50 * i as u64));
            }
            wave.level(7, 8)
        };
        let all_loud = run(&mut wave, [&loud, &loud]);
        let all_quiet = run(&mut wave, [&quiet, &quiet]);
        let mixed = run(&mut wave, [&loud, &quiet]);

        assert!(all_loud > 0.5, "a settled passage should fill: {all_loud}");
        assert!(
            all_quiet < all_loud * 0.5,
            "quiet {all_quiet:.2} against loud {all_loud:.2}"
        );
        assert!(
            mixed < all_loud && mixed > all_quiet,
            "half a word and half a pause read as neither: \
             {all_quiet:.2} / {mixed:.2} / {all_loud:.2}"
        );
    }

    /// The leading edge is the column still being accumulated, and it stays
    /// live across a close — otherwise the edge blanks once every column.
    #[test]
    fn the_live_column_never_blanks() {
        let mut wave = Wave::default();
        let tap = tap_of(&tone(0.5, READ));
        let t0 = Instant::now();
        for frame in 1..=40u64 {
            wave.update(&tap, 8, true, t0 + Duration::from_millis(50 * frame));
            assert!(wave.level(7, 8) > 0.0, "dark edge at frame {frame}");
        }
        assert!(!wave.columns.is_empty(), "no column ever closed");
    }

    /// The picture scrolls in from the right rather than starting full.
    #[test]
    fn history_fills_from_the_right() {
        let mut wave = Wave::default();
        let tap = tap_of(&tone(0.5, READ));
        feed(&mut wave, &tap, 40, Instant::now(), 2);
        assert!(wave.columns.len() < 39, "the test filled the field");
        assert_eq!(wave.level(0, 40), 0.0, "the empty left is not silent");
        assert!(
            wave.level(38, 40) > 0.3,
            "the newest closed column is not against the live one"
        );
    }

    #[test]
    fn the_history_is_capped_at_the_field_width() {
        let wave = settled(&tone(0.5, READ), 12);
        assert_eq!(wave.columns.len(), 11, "the live column has no slot");
    }

    /// The whole point of the picture: a loud passage is visibly taller than a
    /// quiet one. A reference that chased the music would flatten the two into
    /// the same ribbon.
    #[test]
    fn a_quiet_passage_is_shorter_than_a_loud_one() {
        let mut wave = Wave::default();
        let t0 = Instant::now();
        let loud = tap_of(&tone(0.6, READ));
        // About 12 dB down: inside the display window, so the breakdown is
        // short rather than resting on the floor and both assertions bite.
        let quiet = tap_of(&tone(0.15, READ));
        let t = feed(&mut wave, &loud, 40, t0, 30);
        let chorus = wave.level(39, 40);
        feed(&mut wave, &quiet, 40, t, 3);
        let breakdown = wave.level(39, 40);
        assert!(
            breakdown < chorus * 0.4,
            "chorus {chorus:.2} and breakdown {breakdown:.2} read the same"
        );
        // And both are on the same picture: a closed column keeps the height
        // it was written at rather than renormalizing under the new level.
        assert!(
            wave.level(5, 40) > breakdown * 2.0,
            "the history renormalized under the new level"
        );
    }

    /// However the record was mastered. Without the reference a quiet master
    /// would be a flat line down the middle of the field.
    #[test]
    fn the_height_survives_a_quiet_master() {
        let reach = |amp: f32| settled(&tone(amp, READ), 40).level(39, 40);
        let (loud, quiet) = (reach(0.7), reach(0.01));
        assert!(quiet > 0.5, "a quiet master flattened the picture: {quiet}");
        assert!(
            (loud - quiet).abs() < 0.15,
            "loud {loud}, quiet {quiet} — the reference is not normalizing"
        );
    }

    #[test]
    fn silence_reads_as_silence() {
        let wave = settled(&vec![0.0; READ], 20);
        for column in 0..20 {
            assert_eq!(wave.level(column, 20), 0.0, "column {column}");
        }
    }

    /// A stale tap is paused, buffering, or playing on another device. The
    /// picture scrolls flat from the right rather than freezing.
    #[test]
    fn a_stale_tap_scrolls_silence_in() {
        let mut wave = Wave::default();
        let tap = tap_of(&tone(0.6, READ));
        let t0 = Instant::now();
        let t = feed(&mut wave, &tap, 20, t0, 30);
        assert!(wave.level(19, 20) > 0.4);

        for frame in 1..=20u64 {
            wave.update(&tap, 20, false, t + Duration::from_millis(50 * frame));
        }
        assert_eq!(wave.level(19, 20), 0.0, "the live edge kept its level");
        assert!(
            wave.level(5, 20) > 0.4,
            "the picture blanked instead of scrolling off"
        );
    }

    #[test]
    fn column_count_changes_without_panicking() {
        let tap = tap_of(&tone(0.5, READ));
        let mut wave = Wave::default();
        let t0 = Instant::now();
        for (i, columns) in [4usize, 200, 12, 80, 1].into_iter().enumerate() {
            for frame in 0..20u64 {
                wave.update(
                    &tap,
                    columns,
                    true,
                    t0 + Duration::from_millis(1000 * i as u64 + 50 * frame),
                );
            }
            for column in 0..columns {
                let level = wave.level(column, columns);
                assert!((0.0..=1.0).contains(&level), "{columns}: {level}");
            }
        }
    }
}
