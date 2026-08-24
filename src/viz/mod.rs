//! Audio analysis for the player view's visualizer.
//!
//! Everything here reads the newest PCM from the [`AudioTap`] and turns it into
//! something the renderer can paint. The analyzers are split by what they
//! measure rather than by how they look: [`spectrum`] resolves a spectrum,
//! [`wave`] keeps a scrolling history of the signal's loudness, [`scope`]
//! follows the waveform itself, and [`Pulse`] is a single loudness number.
//!
//! [`VizMode`] names the choices; the renderer in [`crate::ui::player`] decides
//! which analyzer a given mode needs, because only it knows the field's shape.
//!
//! Two rules every analyzer here keeps. Decay is driven by elapsed wall clock
//! rather than by frame count, because the draw loop also redraws on input
//! events and frames are not evenly spaced. And a stale tap — paused,
//! buffering, or audio going to another device — falls to rest rather than
//! snapping, so nothing blanks on a single missed frame.

mod scope;
mod spectrum;
mod wave;

pub use scope::Scope;
pub use spectrum::VizState;
pub use wave::Wave;

use std::f32::consts::PI;
use std::time::Instant;

use crate::audio_tap::AudioTap;

pub const SAMPLE_RATE: f32 = 44_100.0;

/// What the player view's field is showing.
///
/// The order is the cycle order, and it runs by how much of the record each
/// one shows: the spectrum at this instant, the last half-minute of loudness,
/// then the waveform itself at a few dozen milliseconds' magnification.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VizMode {
    #[default]
    Bars,
    Wave,
    Scope,
}

impl VizMode {
    pub fn next(self) -> Self {
        match self {
            Self::Bars => Self::Wave,
            Self::Wave => Self::Scope,
            Self::Scope => Self::Bars,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Bars => "bars",
            Self::Wave => "waveform",
            Self::Scope => "scope",
        }
    }
}

/// The chosen mode and every analyzer's rolling state.
///
/// One field on [`AppState`] rather than one per mode: the renderer asks for
/// the analysis the current mode needs and the rest simply do not run. The two
/// that carry a buffer are boxed and built on first use, so cycling past a mode
/// you never look at costs nothing.
///
/// [`AppState`]: crate::app::state::AppState
#[derive(Default)]
pub struct Viz {
    pub mode: VizMode,
    spectrum: VizState,
    wave: Option<Box<Wave>>,
    scope: Option<Box<Scope>>,
}

impl Viz {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cycle(&mut self) -> VizMode {
        self.mode = self.mode.next();
        self.mode
    }

    /// Advance the band envelopes and hand them back, for the modes that draw
    /// the spectrum directly.
    pub fn spectrum(
        &mut self,
        tap: &AudioTap,
        n_bands: usize,
        fresh: bool,
        now: Instant,
    ) -> &VizState {
        self.spectrum.update(tap, n_bands, fresh, now);
        &self.spectrum
    }

    /// Fold this frame's loudness into the scrolling history and hand it back.
    pub fn wave(&mut self, tap: &AudioTap, columns: usize, fresh: bool, now: Instant) -> &Wave {
        let wave = self.wave.get_or_insert_with(Default::default);
        wave.update(tap, columns, fresh, now);
        wave
    }

    /// `columns` counts *pixel* columns, not cells: the scope draws in braille,
    /// which fits two of them across a cell.
    pub fn scope(&mut self, tap: &AudioTap, columns: usize, fresh: bool, now: Instant) -> &Scope {
        let scope = self.scope.get_or_insert_with(Default::default);
        scope.update(tap, columns, fresh, now);
        scope
    }
}

/// Magnitude at a fractional bin index, linearly interpolated.
fn interpolate(mag: &[f32], bin: f32) -> f32 {
    if bin <= 0.0 {
        return mag[0];
    }
    let i = bin.floor() as usize;
    if i + 1 >= mag.len() {
        return *mag.last().unwrap();
    }
    let f = bin - i as f32;
    mag[i] * (1.0 - f) + mag[i + 1] * f
}

/// In-place iterative radix-2 complex FFT. `re` and `im` must be the same
/// power-of-two length.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert_eq!(n, im.len());
    debug_assert!(n.is_power_of_two());
    if n < 2 {
        return;
    }

    // Bit-reversal permutation, tracked incrementally so we never call a
    // reverse-bits helper per element.
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }

    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[b] * cr - im[b] * ci, re[b] * ci + im[b] * cr);
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = (cr * wr - ci * wi, cr * wi + ci * wr);
                cr = next.0;
                ci = next.1;
            }
        }
        len <<= 1;
    }
}

/// Samples the loudness meter reads per frame (~23 ms at 44.1 kHz). Shorter
/// than [`spectrum::WINDOW`]: this is not resolving a spectrum, it is
/// following the
/// envelope, and a longer window smears the transients that make a dot look
/// like it is keeping time.
const PULSE_WINDOW: usize = 1024;

/// Corner frequency of the tilt applied before the level is measured, so the
/// dot lands on the beat rather than on loudness in general.
///
/// One pole, so it is a 6 dB/octave slope and not a wall: a kick dominates,
/// but a snare and a voice still move it, and a record with no low end at all
/// still reads — the reference below normalizes whatever is left. Measuring
/// flat instead tracks *density*, which on a compressed master barely changes
/// from bar to bar and reads as a dot that is merely on.
const PULSE_TILT_HZ: f32 = 250.0;

/// How far below the running reference the meter bottoms out. This is the
/// span the dot's brightness travels over, and it is deliberately narrow:
/// short-term level on a modern master moves within a few dB, and a window
/// wide enough to hold a record's whole dynamic range leaves every beat in
/// the same half-cell of brightness.
const PULSE_RANGE_DB: f32 = 15.0;
/// Expands the bottom of that span, on the same reasoning as [`spectrum::CONTRAST`]:
/// straight normalisation bunches everything near the top, which is what
/// makes a meter read as flat.
const PULSE_CONTRAST: f32 = 1.7;
/// Reference bounds, and how fast it follows. Attack is quick so the first
/// loud bar sets the ceiling; release is slow so the meter does not chase the
/// music's own dynamics and renormalise a quiet passage straight back to
/// full — the balance [`spectrum::AGC_TAU`] documents for the visualizer.
const PULSE_REF_MIN_DB: f32 = -70.0;
const PULSE_REF_MAX_DB: f32 = 0.0;
const PULSE_REF_ATTACK: f32 = 0.20;
const PULSE_REF_RELEASE: f32 = 6.0;
/// Below this the input is treated as true silence, so a muted stream rests
/// at zero instead of having the reference wind the noise floor up to full.
const PULSE_SILENCE: f32 = 1e-4;

/// Envelope time constants. Attack is most of a single frame, so the dot is
/// up on the beat and not after it. Release is shorter than the gap between
/// beats at any tempo you would play a record at — at 120 bpm that gap is
/// 500 ms — because a dot that has not come back down by the next kick never
/// visibly moves.
const PULSE_ATTACK: f32 = 0.04;
const PULSE_RELEASE: f32 = 0.18;

/// A one-number loudness envelope, for the nav row's playing dot.
///
/// Deliberately not a [`VizState`] with one band. The visualizer is only
/// updated while the player view is on screen, and the dot is drawn on both
/// screens — sharing the state would leave the browse screen reading a
/// spectrum that stopped being computed when the player closed. It also does
/// not need one: brightness is a single scalar, and an FFT to produce it would
/// run on every frame of every screen for a value a plain RMS already gives.
pub struct Pulse {
    /// The smoothed level in 0..=1 — what the caller draws.
    level: f32,
    /// Running reference in dB that the window's level is measured against,
    /// so the dot uses its whole travel on a quiet record and on a loud one
    /// alike. Without it the meter reads absolute level: a compressed master
    /// pins near the top and a quiet one never leaves the bottom, and neither
    /// looks like it is moving.
    ref_db: f32,
    /// Wall clock of the previous update, so the envelope decays in real time
    /// rather than per frame. The draw loop redraws on input as well as on the
    /// tick, so frames are not evenly spaced.
    last_update: Option<Instant>,
    samples: Vec<f32>,
}

impl Pulse {
    pub fn new() -> Self {
        Self {
            level: 0.0,
            ref_db: PULSE_REF_MIN_DB,
            last_update: None,
            samples: Vec::new(),
        }
    }

    /// Advance the envelope to `now` and return the level in 0..=1.
    ///
    /// A stale tap (`fresh == false`) means paused, buffering, or audio coming
    /// from another device: nothing is arriving, so the meter falls to rest.
    pub fn update(&mut self, tap: &AudioTap, fresh: bool, now: Instant) -> f32 {
        // Same clamp as `VizState::update`, and for the same reason: the first
        // frame has no reference point, and a long stall must not make the
        // envelope jump a whole period in one step.
        let dt = match self.last_update {
            Some(prev) => now.saturating_duration_since(prev).as_secs_f32().min(0.5),
            None => 0.05,
        };
        self.last_update = Some(now);

        let target = if fresh {
            tap.latest(&mut self.samples, PULSE_WINDOW);
            // A barely-filled ring — the first frames of a track — is not
            // silence, it is "not yet". Rest rather than read it as quiet.
            if self.samples.len() >= 256 {
                self.measure(dt)
            } else {
                0.0
            }
        } else {
            self.samples.clear();
            0.0
        };

        // Asymmetric smoothing: the meter jumps toward a hit and eases away
        // from it. A single time constant either lags the beat or chatters
        // between two of them.
        let tau = if target > self.level {
            PULSE_ATTACK
        } else {
            PULSE_RELEASE
        };
        let k = 1.0 - (-dt / tau).exp();
        self.level += (target - self.level) * k;
        self.level = self.level.clamp(0.0, 1.0);
        self.level
    }

    /// This window's level in 0..=1, measured against the running reference
    /// and advancing it. Silence rests without moving the reference: a track
    /// that ends must not have the gap after it wound up to full.
    fn measure(&mut self, dt: f32) -> f32 {
        let Some(db) = tilted_db(&mut self.samples) else {
            return 0.0;
        };
        let tau = if db > self.ref_db {
            PULSE_REF_ATTACK
        } else {
            PULSE_REF_RELEASE
        };
        self.ref_db += (db - self.ref_db) * (1.0 - (-dt / tau).exp());
        self.ref_db = self.ref_db.clamp(PULSE_REF_MIN_DB, PULSE_REF_MAX_DB);

        let floor = self.ref_db - PULSE_RANGE_DB;
        let norm = ((db - floor) / PULSE_RANGE_DB).clamp(0.0, 1.0);
        norm.powf(PULSE_CONTRAST)
    }
}

impl Default for Pulse {
    fn default() -> Self {
        Self::new()
    }
}

/// Tilted RMS of the window, in dB.
///
/// The tilt is a one-pole lowpass run over the window in place — the samples
/// are a scratch copy of the ring, so filtering them costs nothing and the
/// state need not persist: at [`PULSE_TILT_HZ`] the filter settles in well
/// under a millisecond, far inside the window it is given.
///
/// In dB rather than on the raw amplitude because loudness is perceived that
/// way: a linear meter spends almost all of a record's range in the bottom of
/// its travel and reads as barely moving.
fn tilted_db(samples: &mut [f32]) -> Option<f32> {
    let a = 1.0 - (-2.0 * PI * PULSE_TILT_HZ / SAMPLE_RATE).exp();
    let mut y = 0.0f32;
    let mut sum = 0.0f32;
    for s in samples.iter_mut() {
        y += a * (*s - y);
        sum += y * y;
    }
    let rms = (sum / samples.len() as f32).sqrt();
    (rms > PULSE_SILENCE).then(|| 20.0 * rms.log10())
}

/// Deterministic pink noise via Paul Kellett's 3-pole filter over a seeded
/// LCG. Broadband and comb-free, unlike a sum of tones, so it lights every
/// band — which the visualizer's own rendering tests need as much as this
/// module's do.
#[cfg(test)]
pub fn pink_noise(n: usize) -> Vec<f32> {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let (mut b0, mut b1, mut b2) = (0.0f32, 0.0f32, 0.0f32);
    (0..n)
        .map(|_| {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let white = ((seed >> 33) as f32 / (1u64 << 31) as f32) - 1.0;
            b0 = 0.99765 * b0 + white * 0.0990460;
            b1 = 0.96300 * b1 + white * 0.2965164;
            b2 = 0.57000 * b2 + white * 1.0526913;
            (b0 + b1 + b2 + white * 0.1848) * 0.20
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn fft_of_an_impulse_is_flat() {
        let mut re = vec![0.0f32; 16];
        let mut im = vec![0.0f32; 16];
        re[0] = 1.0;
        fft(&mut re, &mut im);
        for k in 0..16 {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            assert!((mag - 1.0).abs() < 1e-5, "bin {k} = {mag}");
        }
    }

    #[test]
    fn fft_puts_a_sine_in_its_own_bin() {
        const N: usize = 64;
        const K: usize = 7;
        let mut re: Vec<f32> = (0..N)
            .map(|i| (2.0 * PI * K as f32 * i as f32 / N as f32).sin())
            .collect();
        let mut im = vec![0.0f32; N];
        fft(&mut re, &mut im);
        let mags: Vec<f32> = (0..N / 2)
            .map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt())
            .collect();
        let peak = mags
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(peak, K);
        assert!((mags[K] - N as f32 / 2.0).abs() < 1e-3, "{}", mags[K]);
    }

    /// Fill a tap with `signal` and run the meter over it for `secs` of
    /// simulated time, at the fast tick's rate. Returns the settled level.
    /// Program material at a plausible level. [`pink_noise`] is normalized for
    /// the visualizer's AGC, which only cares about the shape of a spectrum,
    /// and comes out around +11 dBFS — far hotter than anything a decoder
    /// hands the tap. The pulse meter reads absolute level, so its tests have
    /// to feed it one: this lands near -15 dBFS, about where a mastered record
    /// sits.
    fn music(scale: f32) -> Vec<f32> {
        pink_noise(PULSE_WINDOW)
            .iter()
            .map(|s| s * 0.05 * scale)
            .collect()
    }

    fn pulse_over(signal: &[f32], secs: u64) -> f32 {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = signal.iter().flat_map(|&s| [s as f64, s as f64]).collect();
        tap.push(&stereo, 1.0);
        let mut pulse = Pulse::new();
        let t0 = Instant::now();
        let mut level = 0.0;
        for frame in 1..=secs * 20 {
            level = pulse.update(&tap, true, t0 + Duration::from_millis(50 * frame));
        }
        level
    }

    /// A kick every `beat_ms` over a quiet bed: a decaying 60 Hz thump, which
    /// is what the dot is meant to land on.
    fn beat_track(secs: f32, beat_ms: u64, amp: f32) -> Vec<f32> {
        let n = (SAMPLE_RATE * secs) as usize;
        let period = (SAMPLE_RATE * beat_ms as f32 / 1000.0) as usize;
        let bed = pink_noise(n);
        (0..n)
            .map(|i| {
                let t = (i % period) as f32 / SAMPLE_RATE;
                let env = (-t / 0.07).exp();
                let kick = env * (2.0 * PI * 60.0 * t).sin();
                amp * (kick + bed[i] * 0.02)
            })
            .collect()
    }

    /// Run the meter over `signal` in real time — 50 ms of new samples into
    /// the tap per frame, the way the fast tick feeds it — and return every
    /// level it reported. Unlike [`pulse_over`], which re-reads one static
    /// ring, this actually moves through the material.
    fn levels_through(signal: &[f32]) -> Vec<f32> {
        const STEP: usize = (SAMPLE_RATE as usize) / 20;
        let tap = AudioTap::new();
        let mut pulse = Pulse::new();
        let t0 = Instant::now();
        signal
            .chunks(STEP)
            .enumerate()
            .map(|(frame, chunk)| {
                let stereo: Vec<f64> = chunk.iter().flat_map(|&s| [s as f64, s as f64]).collect();
                tap.push(&stereo, 1.0);
                pulse.update(
                    &tap,
                    true,
                    t0 + Duration::from_millis(50 * (frame as u64 + 1)),
                )
            })
            .collect()
    }

    /// The whole point of the dot: it has to visibly *move* with the music,
    /// not sit at one brightness for the length of a record. Measured over
    /// the back half, after the reference has settled.
    #[test]
    fn the_meter_swings_across_a_beat() {
        let levels = levels_through(&beat_track(6.0, 500, 0.3));
        let settled = &levels[levels.len() / 2..];
        let lo = settled.iter().copied().fold(f32::MAX, f32::min);
        let hi = settled.iter().copied().fold(f32::MIN, f32::max);
        assert!(hi > 0.75, "the kick should reach the top: {hi}");
        assert!(
            hi - lo > 0.5,
            "the dot barely moves: {lo} to {hi} over {} frames",
            settled.len()
        );
    }

    /// And it swings the same way whatever the master's level, because the
    /// reference normalizes it. Without that the meter reads absolute
    /// loudness: a quiet record never leaves the bottom of its travel and a
    /// compressed one pins at the top, and neither looks like it is moving.
    #[test]
    fn the_swing_survives_a_quiet_master() {
        let swing = |amp: f32| {
            let levels = levels_through(&beat_track(6.0, 500, amp));
            let settled = &levels[levels.len() / 2..];
            settled.iter().copied().fold(f32::MIN, f32::max)
                - settled.iter().copied().fold(f32::MAX, f32::min)
        };
        let loud = swing(0.3);
        let quiet = swing(0.01);
        assert!(quiet > 0.5, "a quiet master flattened the dot: {quiet}");
        assert!(
            (loud - quiet).abs() < 0.25,
            "loud {loud}, quiet {quiet} — the reference is not normalizing"
        );
    }

    #[test]
    fn silence_rests_the_meter() {
        assert_eq!(pulse_over(&vec![0.0; PULSE_WINDOW], 2), 0.0);
    }

    /// A stale tap is paused, buffering, or playing on another device.
    /// Whichever it is, nothing is arriving, so the meter falls to rest —
    /// and does it from wherever it was, rather than snapping.
    #[test]
    fn a_stale_tap_falls_rather_than_snapping() {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = music(1.0)
            .iter()
            .flat_map(|&s| [s as f64, s as f64])
            .collect();
        tap.push(&stereo, 1.0);
        let mut pulse = Pulse::new();
        let t0 = Instant::now();
        for frame in 1..=40u64 {
            pulse.update(&tap, true, t0 + Duration::from_millis(50 * frame));
        }
        let lit = pulse.update(&tap, true, t0 + Duration::from_millis(2050));
        assert!(lit > 0.7, "{lit}");

        // One frame of staleness must not blank it.
        let next = pulse.update(&tap, false, t0 + Duration::from_millis(2100));
        assert!(next < lit && next > 0.3, "snapped to {next} from {lit}");
        // Given a second it is at rest.
        let mut level = next;
        for frame in 43..=60u64 {
            level = pulse.update(&tap, false, t0 + Duration::from_millis(50 * frame));
        }
        assert!(level < 0.1, "{level}");
    }

    /// The envelope is driven by elapsed time, not by frame count, because
    /// the loop redraws on input as well as on the tick — the same property
    /// `decay_is_time_based_not_frame_based` asserts for the visualizer.
    #[test]
    fn the_envelope_is_time_based_not_frame_based() {
        let signal = music(1.0);
        let tap = AudioTap::new();
        let stereo: Vec<f64> = signal.iter().flat_map(|&s| [s as f64, s as f64]).collect();
        tap.push(&stereo, 1.0);

        let settle = |step_ms: u64, frames: u64| {
            let mut pulse = Pulse::new();
            let t0 = Instant::now();
            let mut level = 0.0;
            for frame in 1..=frames {
                level = pulse.update(&tap, true, t0 + Duration::from_millis(step_ms * frame));
            }
            level
        };
        // One second of simulated time either way: 20 frames at 50 ms or 4 at
        // 250 ms. Same answer, whatever the tick.
        let fast = settle(50, 20);
        let slow = settle(250, 4);
        assert!((fast - slow).abs() < 0.05, "fast {fast}, slow {slow}");
    }
}
