//! Spectrum analysis for the player view's visualizer.
//!
//! Reads the newest PCM from the [`AudioTap`], runs a windowed FFT, folds the
//! bins into log-spaced bands, and drives the three animation envelopes the
//! renderer draws: the bar itself, a slower-decaying afterglow above it, and a
//! peak-hold cap.
//!
//! The band mapping is tuned for *looks*, not measurement. Two deliberate
//! distortions: each band sums the power of every bin it covers (so broadband
//! treble accumulates instead of being sampled at one point), and a pink tilt
//! lifts the high end to offset music's natural spectral rolloff. Without
//! either, the bass pins at full height and the top third never lights up.

use std::f32::consts::PI;
use std::time::Instant;

use crate::audio_tap::AudioTap;

/// Samples analyzed per frame (~46 ms at 44.1 kHz).
const WINDOW: usize = 2048;
/// The windowed samples are zero-padded to this length before the transform.
/// Interpolating the spectrum this way keeps the 46 ms time response — so kick
/// drums stay punchy — while halving the effective bin spacing to ~10.8 Hz,
/// which is what keeps the lowest bands from collapsing onto the same bin.
const FFT_N: usize = 4096;
const SAMPLE_RATE: f32 = 44_100.0;

/// Analyzed spectrum span. The top end stays inside the Ogg lowpass Spotify's
/// streams are encoded with; probing above it would render as dead bars.
const FREQ_LO: f32 = 40.0;
const FREQ_HI: f32 = 14_000.0;

/// Spectral tilt in dB per decade of frequency, applied before display.
///
/// Summing each band's bins already leaves genuinely pink content reading
/// flat, so this is not compensating the whole 3-6 dB/octave rolloff — only
/// the part by which real program material falls *below* pink at the top of
/// the spectrum. The main knob for "does the high end look alive"; push it
/// much past this and quiet cymbals start out-reading kick drums.
const TILT_DB_PER_DECADE: f32 = 4.5;

/// Display window below the running reference level. Widening this pushes
/// every band *up* toward the ceiling, which turns the bottom rows into a
/// permanent slab with no motion in them; this is deliberately tight.
const DB_RANGE: f32 = 34.0;
/// Expands the bottom of the level range so quiet bands fall to the floor
/// instead of hovering mid-pane. On real music most bands land within 15-20 dB
/// of the loudest, and straight normalisation leaves them all bunched in the
/// top half — which reads as a rolling hillside rather than as bars.
const CONTRAST: f32 = 1.8;
/// Bounds on the AGC reference so silence can't wind the gain to infinity and
/// a loud master can't push the whole spectrum off the top.
/// The upper bound sits well above 0 dBFS because the tilt adds up to ~38 dB
/// at the top of the analyzed span.
const AGC_MIN_DB: f32 = -50.0;
const AGC_MAX_DB: f32 = 30.0;
/// AGC release, and the balance between "quiet parts look quiet" and "the
/// display always has something going on".
///
/// A few seconds is too short: the reference chases the music's own dynamics
/// and every quiet passage renormalises straight back to full scale. Half a
/// minute is too long in the other direction — quiet stretches go flat and
/// stay there, and the whole thing reads as dead. This sits between the two:
/// a breakdown visibly drops for several seconds before the display opens
/// back up. Attack ([`AGC_ATTACK_TAU`]) stays fast either way, so loud hits
/// still set the ceiling at once.
const AGC_TAU: f32 = 10.0;

/// Envelope release time constants, in seconds. The glow lags the bar by
/// enough to leave a row or two of afterglow behind it as it drops; much
/// longer and the trail detaches into speckle floating over the field.
const TAU_BAR: f32 = 0.32;
const TAU_GLOW: f32 = 0.60;
/// AGC attack. Fast, but not instantaneous: slamming the reference to a single
/// transient's loudest band squashes the rest of the spectrum for a frame.
const AGC_ATTACK_TAU: f32 = 0.15;

/// Below this a band is treated as fully at rest, so idle frames settle to
/// exactly zero instead of decaying forever.
const SILENCE: f32 = 0.004;

/// Rolling visualizer state: the analyzer's scratch space, the AGC reference,
/// and one animation envelope per band.
pub struct VizState {
    bars: Vec<f32>,
    glow: Vec<f32>,
    /// Running reference level in dB that band levels are measured against.
    agc_db: f32,
    /// Wall clock of the previous update; decay is time-based because the
    /// draw loop also redraws on input events, not just on the frame tick.
    last_update: Option<Instant>,
    samples: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    mag: Vec<f32>,
    targets: Vec<f32>,
}

impl VizState {
    pub fn new() -> Self {
        Self {
            bars: Vec::new(),
            glow: Vec::new(),
            agc_db: AGC_MIN_DB,
            last_update: None,
            samples: Vec::new(),
            re: vec![0.0; FFT_N],
            im: vec![0.0; FFT_N],
            mag: vec![0.0; FFT_N / 2 + 1],
            targets: Vec::new(),
        }
    }

    pub fn bars(&self) -> &[f32] {
        &self.bars
    }

    pub fn glow(&self) -> &[f32] {
        &self.glow
    }

    /// Analyze the tap and advance every envelope to `now`. A stale tap
    /// (`fresh == false`) skips analysis and just lets the bars fall.
    pub fn update(&mut self, tap: &AudioTap, n_bands: usize, fresh: bool, now: Instant) {
        self.resize(n_bands);

        // First frame has no reference point; treat it as a single tick so
        // nothing jumps.
        let dt = match self.last_update {
            Some(prev) => now.saturating_duration_since(prev).as_secs_f32().min(0.5),
            None => 0.05,
        };
        self.last_update = Some(now);

        if fresh {
            tap.latest(&mut self.samples, WINDOW);
        } else {
            self.samples.clear();
        }
        // A barely-filled ring (just after a track starts) has nothing worth
        // transforming; let the bars rest rather than analyze noise.
        if self.samples.len() >= 256 {
            self.analyze(n_bands, dt);
        } else {
            self.targets.iter_mut().for_each(|t| *t = 0.0);
        }

        self.advance(dt);
    }

    fn resize(&mut self, n_bands: usize) {
        self.bars.resize(n_bands, 0.0);
        self.glow.resize(n_bands, 0.0);
        self.targets.resize(n_bands, 0.0);
    }

    /// Fill `self.targets` with this frame's band levels in 0..=1.
    fn analyze(&mut self, n_bands: usize, dt: f32) {
        let n = self.samples.len();

        // Hann window, then zero-pad. Without the window a single loud bass
        // tone leaks into every neighbouring band and widens the bass wall.
        self.re[..n]
            .iter_mut()
            .zip(&self.samples)
            .enumerate()
            .for_each(|(i, (dst, &x))| {
                let w = 0.5 * (1.0 - (2.0 * PI * i as f32 / (n - 1) as f32).cos());
                *dst = x * w;
            });
        self.re[n..].iter_mut().for_each(|v| *v = 0.0);
        self.im.iter_mut().for_each(|v| *v = 0.0);

        fft(&mut self.re, &mut self.im);

        // Hann halves the coherent gain, and only half the spectrum is kept,
        // so a full-scale sine lands near 0 dBFS.
        let scale = 4.0 / n as f32;
        for (k, m) in self.mag.iter_mut().enumerate() {
            *m = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt() * scale;
        }

        let bin_hz = SAMPLE_RATE / FFT_N as f32;
        let top_bin = self.mag.len() - 1;
        let mut frame_max_db = AGC_MIN_DB;

        for k in 0..n_bands {
            let lo = band_edge(k, n_bands);
            let hi = band_edge(k + 1, n_bands);

            // Sum the power of every bin the band covers. This is what lets
            // broadband treble register at all: its energy is spread over
            // hundreds of Hz rather than concentrated on one frequency.
            let first = ((lo / bin_hz).ceil() as usize).min(top_bin);
            let last = ((hi / bin_hz).floor() as usize).min(top_bin);
            let power: f32 = if last >= first {
                self.mag[first..=last].iter().map(|m| m * m).sum()
            } else {
                0.0
            };

            // At the very bottom a band can still be narrower than one bin.
            // Interpolating at the center keeps it from reading as silence
            // purely because no bin center happened to fall inside it.
            let center = (lo * hi).sqrt();
            let interp = interpolate(&self.mag, center / bin_hz);
            let amp = power.sqrt().max(interp);

            let tilt = TILT_DB_PER_DECADE * (center / FREQ_LO).log10();
            let db = 20.0 * (amp + 1e-9).log10() + tilt;
            frame_max_db = frame_max_db.max(db);
            self.targets[k] = db;
        }

        // Fast attack, slow release on the reference level: transients set the
        // ceiling quickly, quiet passages open the gain back up gradually.
        let tau = if frame_max_db > self.agc_db {
            AGC_ATTACK_TAU
        } else {
            AGC_TAU
        };
        let a = (-dt / tau).exp();
        self.agc_db = (self.agc_db * a + frame_max_db * (1.0 - a)).clamp(AGC_MIN_DB, AGC_MAX_DB);

        let floor = self.agc_db - DB_RANGE;
        for t in self.targets.iter_mut() {
            *t = ((*t - floor) / DB_RANGE).clamp(0.0, 1.0).powf(CONTRAST);
        }
    }

    /// Advance the bar and glow envelopes by `dt` seconds toward the
    /// freshly-computed targets. Attack is instant on both; only the release
    /// is smoothed, which is what gives the Winamp-ish snap.
    fn advance(&mut self, dt: f32) {
        let (fall_bar, fall_glow) = ((-dt / TAU_BAR).exp(), (-dt / TAU_GLOW).exp());

        for i in 0..self.bars.len() {
            let target = self.targets[i];

            // `>=`, not `>`: on a steady band target and bar are equal every
            // frame, and a strict test would take the decay branch each time,
            // fluttering the bar by the fall factor forever.
            self.bars[i] = rest(if target >= self.bars[i] {
                target
            } else {
                self.bars[i] * fall_bar
            });

            // The glow never drops below the bar, so `bar..glow` is exactly
            // the ground the bar has just given up — the fade chases it down.
            self.glow[i] = rest((self.glow[i] * fall_glow).max(self.bars[i]));
        }
    }
}

impl Default for VizState {
    fn default() -> Self {
        Self::new()
    }
}

fn rest(v: f32) -> f32 {
    if v < SILENCE { 0.0 } else { v }
}

/// Lower edge of band `k` of `n`, log-spaced across the analyzed span.
/// Passing `k == n` gives the top edge of the last band.
fn band_edge(k: usize, n: usize) -> f32 {
    FREQ_LO * (FREQ_HI / FREQ_LO).powf(k as f32 / n as f32)
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
/// than [`WINDOW`]: this is not resolving a spectrum, it is following the
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
/// Expands the bottom of that span, on the same reasoning as [`CONTRAST`]:
/// straight normalisation bunches everything near the top, which is what
/// makes a meter read as flat.
const PULSE_CONTRAST: f32 = 1.7;
/// Reference bounds, and how fast it follows. Attack is quick so the first
/// loud bar sets the ceiling; release is slow so the meter does not chase the
/// music's own dynamics and renormalise a quiet passage straight back to
/// full — the balance [`AGC_TAU`] documents for the visualizer.
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

    /// Drive `VizState` with a signal already sitting in a tap, and return the
    /// settled bar levels. The AGC reference starts at its floor and has to
    /// converge, so a single frame would report every band clamped at 1.0.
    fn levels_for(signal: &[f32], n_bands: usize) -> Vec<f32> {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = signal.iter().flat_map(|&s| [s as f64, s as f64]).collect();
        tap.push(&stereo, 1.0);
        let mut viz = VizState::new();
        let t0 = Instant::now();
        for frame in 0..60u64 {
            viz.update(&tap, n_bands, true, t0 + Duration::from_millis(50 * frame));
        }
        viz.bars().to_vec()
    }

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * PI * freq * i as f32 / SAMPLE_RATE).sin())
            .collect()
    }

    fn mean(v: &[f32]) -> f32 {
        v.iter().sum::<f32>() / v.len() as f32
    }

    /// Swap the tap's contents for `signal`, then run the visualizer over it
    /// for `secs` of simulated time starting at `from`.
    fn feed(viz: &mut VizState, signal: &[f32], from: Instant, secs: u64) -> Instant {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = signal.iter().flat_map(|&s| [s as f64, s as f64]).collect();
        tap.push(&stereo, 1.0);
        let frames = secs * 20;
        for frame in 1..=frames {
            viz.update(&tap, 24, true, from + Duration::from_millis(50 * frame));
        }
        from + Duration::from_millis(50 * frames)
    }

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

    #[test]
    fn a_sine_peaks_in_the_band_that_contains_it() {
        let n_bands = 32;
        let freq = 1000.0;
        let bands = levels_for(&sine(freq, 0.5, WINDOW), n_bands);
        let peak = bands
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert!(
            band_edge(peak, n_bands) <= freq && freq < band_edge(peak + 1, n_bands),
            "peak band {peak} spans {}..{} Hz, not {freq}",
            band_edge(peak, n_bands),
            band_edge(peak + 1, n_bands)
        );
        assert!(bands[peak] > 0.7, "peak too low: {}", bands[peak]);
    }

    #[test]
    fn silence_rests_every_band() {
        assert!(levels_for(&vec![0.0; WINDOW], 24).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn a_stale_tap_leaves_the_bars_at_rest() {
        let tap = AudioTap::new();
        tap.push(&vec![0.9; WINDOW * 2], 1.0);
        let mut viz = VizState::new();
        viz.update(&tap, 16, false, Instant::now());
        assert!(viz.bars().iter().all(|&v| v == 0.0));
        assert_eq!(viz.bars().len(), 16);
    }

    /// The regression test for the original complaint: on broadband content
    /// the whole spectrum has to be alive, not a pinned bass wall over dead
    /// treble. The tilt leaves the display sloping slightly upward on true
    /// pink, which is the headroom real music's HF rolloff eats into.
    #[test]
    fn pink_noise_lights_the_whole_spectrum() {
        let n = 24;
        let bands = levels_for(&pink_noise(WINDOW), n);
        let (low, high) = (mean(&bands[..n / 3]), mean(&bands[2 * n / 3..]));
        assert!(
            high > 0.55,
            "high end is dead: low {low:.2}, high {high:.2}, bands {bands:?}"
        );
        assert!(
            low > 0.25,
            "low end fell off the floor: low {low:.2}, bands {bands:?}"
        );
        // The AGC references the loudest band, so exactly one should touch
        // the ceiling; more than that means the spectrum is clipping.
        assert_eq!(
            bands.iter().filter(|&&b| b >= 1.0).count(),
            1,
            "bands are pinned at full scale: {bands:?}"
        );
    }

    /// The wall-of-green guard. Broadband content leaves most bands within
    /// 15-20 dB of the loudest; without the contrast expander they all map
    /// into the top half of the pane and the field reads as a rolling hill
    /// instead of as bars.
    #[test]
    fn levels_span_the_full_height_not_just_the_top() {
        // A quiet broadband bed with a few loud tones over it — the shape
        // real music has. Straight normalisation floats the bed into the top
        // half, leaving the tones nothing to stand out from.
        let mut signal = pink_noise(WINDOW);
        signal.iter_mut().for_each(|s| *s *= 0.10);
        for f in [90.0, 700.0, 5000.0] {
            for (s, t) in signal.iter_mut().zip(sine(f, 0.45, WINDOW)) {
                *s += t;
            }
        }
        let bands = levels_for(&signal, 32);
        let quiet = bands.iter().filter(|&&b| b < 0.25).count();
        let tall = bands.iter().filter(|&&b| b > 0.75).count();
        assert!(
            quiet >= 6,
            "only {quiet} bands near the floor \u{2014} the wall is back: {bands:?}"
        );
        assert!(tall >= 2, "only {tall} bands near the ceiling: {bands:?}");
    }

    /// `AGC_TAU` balances two failure modes, and this pins both ends of it.
    /// Too short and the reference chases the music's own dynamics, so a quiet
    /// passage renormalises straight back to full scale and nothing ever looks
    /// quiet. Too long and quiet stretches go flat and stay there, and the
    /// display reads as dead. A breakdown should drop, hold for several
    /// seconds, then open back up.
    #[test]
    fn a_quiet_passage_drops_before_the_display_opens_back_up() {
        let loud = pink_noise(WINDOW);
        // 20 dB down.
        let quiet: Vec<f32> = loud.iter().map(|s| s * 0.1).collect();

        let mut viz = VizState::new();
        let t = feed(&mut viz, &loud, Instant::now(), 30);
        let chorus = mean(viz.bars());
        assert!(chorus > 0.4, "the loud passage never lit up: {chorus:.2}");

        let t = feed(&mut viz, &quiet, t, 5);
        let early = mean(viz.bars());
        assert!(
            early < chorus * 0.75,
            "quiet passage barely dropped: {early:.2} vs chorus {chorus:.2}"
        );

        feed(&mut viz, &quiet, t, 25);
        let late = mean(viz.bars());
        assert!(
            late > early * 1.15,
            "still flat half a minute in \u{2014} too dead: {late:.2} vs {early:.2}"
        );
    }

    /// The far end of the same trade: however the release is tuned, a track
    /// that is simply mastered quietly must not stay dark for its whole length.
    #[test]
    fn a_quiet_track_recovers_over_its_own_length() {
        let quiet: Vec<f32> = pink_noise(WINDOW).iter().map(|s| s * 0.1).collect();
        let mut viz = VizState::new();
        let t = feed(&mut viz, &pink_noise(WINDOW), Instant::now(), 30);
        feed(&mut viz, &quiet, t, 150);
        let settled = mean(viz.bars());
        assert!(
            settled > 0.4,
            "still dark after two and a half minutes: {settled:.2}"
        );
    }

    /// The tilt lifts the high end, but it must not invent content that
    /// isn't there — a bass-only signal still leaves the top bands dark.
    #[test]
    fn bass_only_content_leaves_the_top_dark() {
        let mut signal = sine(60.0, 0.6, WINDOW);
        for (s, h) in signal.iter_mut().zip(sine(120.0, 0.3, WINDOW)) {
            *s += h;
        }
        let n = 24;
        let bands = levels_for(&signal, n);
        assert!(
            bands[..n / 4].iter().any(|&b| b > 0.6),
            "bass is missing: {bands:?}"
        );
        assert!(
            bands[2 * n / 3..].iter().all(|&b| b < 0.15),
            "the tilt invented treble: {bands:?}"
        );
    }

    #[test]
    fn decay_is_time_based_not_frame_based() {
        // Same elapsed time, different frame counts, must land in the same
        // place — the draw loop redraws on input events too, so frame count
        // is not a clock.
        let settle = |steps: u32, ms: u64| {
            let tap = AudioTap::new();
            tap.push(&vec![0.9f64; WINDOW * 2], 1.0);
            let mut viz = VizState::new();
            let t0 = Instant::now();
            viz.update(&tap, 8, true, t0);
            let peak = viz.bars()[0];
            assert!(peak > 0.0);
            tap.clear();
            for i in 1..=steps {
                viz.update(&tap, 8, false, t0 + Duration::from_millis(ms * i as u64));
            }
            viz.bars()[0] / peak
        };
        let coarse = settle(2, 100);
        let fine = settle(8, 25);
        assert!(
            (coarse - fine).abs() < 0.02,
            "frame-rate dependent decay: {coarse} vs {fine}"
        );
    }

    /// The renderer draws `bar..glow` as afterglow, so the glow must never
    /// dip below the bar and must lag behind it on the way down.
    #[test]
    fn the_glow_chases_a_falling_bar_down() {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = sine(500.0, 0.9, WINDOW)
            .iter()
            .flat_map(|&s| [s as f64, s as f64])
            .collect();
        tap.push(&stereo, 1.0);
        let mut viz = VizState::new();
        let t0 = Instant::now();
        viz.update(&tap, 16, true, t0);
        tap.clear();

        let mut trailed = false;
        for ms in [100u64, 300, 600, 1000, 2000] {
            viz.update(&tap, 16, false, t0 + Duration::from_millis(ms));
            for b in 0..16 {
                assert!(
                    viz.bars()[b] <= viz.glow()[b],
                    "glow fell below the bar at {ms} ms, band {b}: {} / {}",
                    viz.bars()[b],
                    viz.glow()[b]
                );
            }
            trailed |= viz.glow().iter().zip(viz.bars()).any(|(g, b)| g > b);
        }
        assert!(trailed, "the glow never trailed above a falling bar");
        // And it settles to rest rather than decaying forever. Steps stay
        // under the 0.5 s dt clamp, which a single long jump would trip.
        for step in 5..20u64 {
            viz.update(&tap, 16, false, t0 + Duration::from_millis(400 * step));
        }
        assert!(viz.glow().iter().all(|&g| g == 0.0), "{:?}", viz.glow());
    }

    #[test]
    fn band_count_changes_without_panicking() {
        let tap = AudioTap::new();
        tap.push(&vec![0.3f64; WINDOW * 2], 1.0);
        let mut viz = VizState::new();
        let t0 = Instant::now();
        for (i, n) in [4usize, 64, 8, 48, 1].into_iter().enumerate() {
            viz.update(&tap, n, true, t0 + Duration::from_millis(50 * i as u64));
            assert_eq!(viz.bars().len(), n);
            assert_eq!(viz.glow().len(), n);
        }
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
