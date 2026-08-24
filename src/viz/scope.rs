//! Triggered waveform, for the oscilloscope mode.
//!
//! No transform: this is the PCM itself. Two things stand between the tap and
//! something worth looking at.
//!
//! A trigger, because a window taken at an arbitrary offset starts at an
//! arbitrary point in the waveform, and the trace then skitters sideways every
//! frame. Starting each frame at a rising zero-crossing holds a steady tone
//! still on screen.
//!
//! And a running amplitude reference, for the reason [`Pulse`] carries one: a
//! trace drawn against absolute full scale leaves a quietly-mastered record as
//! a flat line down the middle of the field.
//!
//! [`Pulse`]: super::Pulse

use std::time::Instant;

use crate::audio_tap::AudioTap;

/// Samples pulled from the tap. The trace itself is [`SPAN`] of them; the rest
/// is the room the trigger has to search backward through.
const READ: usize = 3072;
/// Samples the field spans, ~23 ms at 44.1 kHz. Wide enough to hold a couple of
/// cycles of a bass note, narrow enough that a hi-hat is still a shape rather
/// than a solid band.
const SPAN: usize = 1024;

/// Reference attack and release, in seconds. Attack is quick so the first loud
/// bar sets the height; release is slow so the trace does not breathe with the
/// music's own dynamics.
const REF_ATTACK: f32 = 0.08;
const REF_RELEASE: f32 = 2.5;
/// Floor on the reference, so silence cannot wind the gain up onto the noise
/// under it.
const REF_MIN: f32 = 2e-3;
/// Headroom left above the reference, so a peak louder than the recent ones
/// still lands inside the field instead of being clipped flat against its edge.
const HEADROOM: f32 = 1.2;

/// How far from zero a sample must travel to count as a crossing, as a
/// fraction of the reference. Without it the trigger latches onto whatever
/// noise happens to sit nearest zero and the trace jitters anyway.
const TRIGGER_HYSTERESIS: f32 = 0.06;

/// Release when the tap goes stale, in seconds. The trace collapses to the
/// centerline rather than vanishing on the frame audio stops.
const REST_TAU: f32 = 0.25;

/// The trace, as one vertical extent per pixel column.
#[derive(Default)]
pub struct Scope {
    /// `(low, high)` per column in -1..=1, oldest sample leftmost. A column is
    /// the extent of every sample that falls in it rather than one sampled
    /// point: decimating instead would alias high frequencies into whatever
    /// phantom tone the column spacing happened to beat against.
    trace: Vec<(f32, f32)>,
    /// Running peak the trace is drawn against.
    reference: f32,
    last_update: Option<Instant>,
    samples: Vec<f32>,
}

impl Scope {
    /// Read the tap and lay this frame's waveform out over `columns` pixel
    /// columns. A stale tap (`fresh == false`) collapses the trace toward the
    /// centerline instead.
    pub fn update(&mut self, tap: &AudioTap, columns: usize, fresh: bool, now: Instant) {
        // The same clamp the other analyzers use, for the same reason: the
        // first frame has no reference point, and a long stall must not
        // advance an envelope by a whole period in one step.
        let dt = match self.last_update {
            Some(prev) => now.saturating_duration_since(prev).as_secs_f32().min(0.5),
            None => 0.05,
        };
        self.last_update = Some(now);
        self.trace.resize(columns, (0.0, 0.0));

        if fresh {
            tap.latest(&mut self.samples, READ);
        } else {
            self.samples.clear();
        }
        // A barely-filled ring is the first frames of a track, not silence.
        if self.samples.len() < SPAN {
            self.rest(dt);
            return;
        }

        self.advance_reference(dt);
        let start = self.trigger();
        let scale = 1.0 / (self.reference * HEADROOM);
        let window = &self.samples[start..start + SPAN];
        for (i, slot) in self.trace.iter_mut().enumerate() {
            let lo = i * SPAN / columns;
            let hi = ((i + 1) * SPAN / columns).max(lo + 1);
            let (mut low, mut high) = (f32::MAX, f32::MIN);
            for &s in &window[lo..hi] {
                low = low.min(s);
                high = high.max(s);
            }
            *slot = (
                (low * scale).clamp(-1.0, 1.0),
                (high * scale).clamp(-1.0, 1.0),
            );
        }
    }

    pub fn trace(&self) -> &[(f32, f32)] {
        &self.trace
    }

    fn rest(&mut self, dt: f32) {
        let k = (-dt / REST_TAU).exp();
        for (low, high) in self.trace.iter_mut() {
            *low *= k;
            *high *= k;
        }
    }

    fn advance_reference(&mut self, dt: f32) {
        let peak = self
            .samples
            .iter()
            .fold(0.0f32, |acc, s| acc.max(s.abs()))
            .max(REF_MIN);
        let tau = if peak > self.reference {
            REF_ATTACK
        } else {
            REF_RELEASE
        };
        self.reference += (peak - self.reference) * (1.0 - (-dt / tau).exp());
        self.reference = self.reference.max(REF_MIN);
    }

    /// Where the drawn window starts: the most recent rising zero-crossing
    /// that still leaves [`SPAN`] samples behind it.
    ///
    /// Most recent rather than earliest, so the trace shows the newest audio
    /// the ring holds. With no crossing to find — a DC-ish or silent window —
    /// the newest full span is taken as-is, which is the same picture a
    /// scope's free-run mode gives.
    ///
    /// The arming is what makes [`TRIGGER_HYSTERESIS`] a hysteresis rather than
    /// a threshold: a crossing only counts once the signal has been below
    /// `-level` since the previous one, so noise sitting near zero cannot fire
    /// it every sample. Testing both sides of one step against the band instead
    /// would want the signal to jump the whole band between two samples, which
    /// nothing at an audible frequency does.
    fn trigger(&self) -> usize {
        let free_run = self.samples.len() - SPAN;
        let level = self.reference * TRIGGER_HYSTERESIS;
        let mut armed = false;
        let mut last = None;
        for (i, &s) in self.samples[..free_run].iter().enumerate() {
            if s < -level {
                armed = true;
            } else if armed && s >= 0.0 {
                last = Some(i);
                armed = false;
            }
        }
        last.unwrap_or(free_run)
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::PI;
    use std::time::Duration;

    use super::*;
    use crate::viz::SAMPLE_RATE;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * PI * freq * i as f32 / SAMPLE_RATE).sin())
            .collect()
    }

    fn tap_of(signal: &[f32]) -> AudioTap {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = signal.iter().flat_map(|&s| [s as f64, s as f64]).collect();
        tap.push(&stereo, 1.0);
        tap
    }

    /// Run the scope over a static tap until its reference settles.
    fn settled(signal: &[f32], columns: usize) -> Scope {
        let tap = tap_of(signal);
        let mut scope = Scope::default();
        let t0 = Instant::now();
        for frame in 0..60u64 {
            scope.update(&tap, columns, true, t0 + Duration::from_millis(50 * frame));
        }
        scope
    }

    /// The whole point of the trigger: a steady tone must land in the same
    /// place every frame, whatever the ring's fill happens to be.
    #[test]
    fn a_steady_tone_holds_still() {
        let tap = AudioTap::new();
        let mut scope = Scope::default();
        let t0 = Instant::now();
        let tone = sine(440.0, 0.6, READ * 4);
        let mut traces = Vec::new();
        // Push a ragged number of samples between frames, so the untriggered
        // window would start at a different phase each time.
        for (frame, chunk) in tone.chunks(517).enumerate().take(40) {
            let stereo: Vec<f64> = chunk.iter().flat_map(|&s| [s as f64, s as f64]).collect();
            tap.push(&stereo, 1.0);
            scope.update(
                &tap,
                64,
                true,
                t0 + Duration::from_millis(50 * (frame as u64 + 1)),
            );
            traces.push(scope.trace().to_vec());
        }
        let last = traces.last().unwrap();
        let prev = &traces[traces.len() - 2];
        let drift: f32 = last
            .iter()
            .zip(prev)
            .map(|(a, b)| (a.1 - b.1).abs())
            .fold(0.0, f32::max);
        assert!(drift < 0.15, "the trace moved between frames: {drift}");
    }

    /// And it uses the field: a trace that fills a tenth of the height reads
    /// as a flat line.
    #[test]
    fn the_trace_fills_the_field() {
        let scope = settled(&sine(220.0, 0.5, READ), 64);
        let high = scope.trace().iter().fold(f32::MIN, |a, s| a.max(s.1));
        let low = scope.trace().iter().fold(f32::MAX, |a, s| a.min(s.0));
        assert!(high > 0.6, "the trace never reached up: {high}");
        assert!(low < -0.6, "the trace never reached down: {low}");
    }

    /// However the record was mastered. Without the reference a quiet one is a
    /// flat line down the middle of the field.
    #[test]
    fn the_height_survives_a_quiet_master() {
        let reach = |amp: f32| {
            settled(&sine(220.0, amp, READ), 64)
                .trace()
                .iter()
                .fold(f32::MIN, |a, s| a.max(s.1))
        };
        let (loud, quiet) = (reach(0.7), reach(0.01));
        assert!(quiet > 0.6, "a quiet master flattened the trace: {quiet}");
        assert!(
            (loud - quiet).abs() < 0.2,
            "loud {loud}, quiet {quiet} — the reference is not normalizing"
        );
    }

    /// A column is the extent of the samples under it, so content above the
    /// column rate reads as a solid band rather than aliasing into a phantom
    /// low tone.
    #[test]
    fn content_above_the_column_rate_stays_a_band() {
        // 8 kHz over 64 columns: ~5 cycles inside a single column.
        let scope = settled(&sine(8000.0, 0.5, READ), 64);
        let thin = scope
            .trace()
            .iter()
            .filter(|(low, high)| high - low < 0.5)
            .count();
        assert!(thin < 8, "{thin} of 64 columns collapsed to a line");
    }

    #[test]
    fn silence_rests_the_trace() {
        let scope = settled(&vec![0.0; READ], 64);
        assert!(
            scope
                .trace()
                .iter()
                .all(|&(low, high)| low == 0.0 && high == 0.0),
            "{:?}",
            scope.trace()
        );
    }

    /// A stale tap is paused, buffering, or playing on another device. The
    /// trace collapses to the centerline rather than blanking on one frame.
    #[test]
    fn a_stale_tap_collapses_rather_than_snapping() {
        let tap = tap_of(&sine(220.0, 0.6, READ));
        let mut scope = Scope::default();
        let t0 = Instant::now();
        for frame in 0..60u64 {
            scope.update(&tap, 64, true, t0 + Duration::from_millis(50 * frame));
        }
        let lit = scope.trace().iter().fold(f32::MIN, |a, s| a.max(s.1));
        assert!(lit > 0.6, "{lit}");

        scope.update(&tap, 64, false, t0 + Duration::from_millis(3050));
        let next = scope.trace().iter().fold(f32::MIN, |a, s| a.max(s.1));
        assert!(next < lit && next > 0.3, "snapped to {next} from {lit}");

        for frame in 62..=90u64 {
            scope.update(&tap, 64, false, t0 + Duration::from_millis(50 * frame));
        }
        let level = scope.trace().iter().fold(f32::MIN, |a, s| a.max(s.1));
        assert!(level < 0.05, "{level}");
    }

    #[test]
    fn column_count_changes_without_panicking() {
        let tap = tap_of(&sine(440.0, 0.5, READ));
        let mut scope = Scope::default();
        let t0 = Instant::now();
        for (i, columns) in [8usize, 256, 16, 160, 1].into_iter().enumerate() {
            scope.update(
                &tap,
                columns,
                true,
                t0 + Duration::from_millis(50 * i as u64),
            );
            assert_eq!(scope.trace().len(), columns);
        }
    }
}
