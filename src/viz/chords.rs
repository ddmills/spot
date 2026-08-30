//! Chroma analysis and chord naming, for the chord mode.
//!
//! The other analyzers measure the signal. This one reads the music out of it:
//! how much of each of the twelve pitch classes is sounding, and which chord
//! that pattern is.
//!
//! Three steps. A long transform resolves individual semitones. Each semitone
//! is then scored against its own harmonic series, which is what keeps a
//! minor chord minor — a note's fifth harmonic is its major third, so a
//! detector that reads peaks alone hears a major chord under every minor one.
//! The twelve pitch classes those saliences fold into are finally correlated
//! against a chord dictionary.
//!
//! Correlation rather than plain similarity, because a flat chroma — a drum
//! break, a noise floor — overlaps every chord template equally well and would
//! otherwise score as high as a real chord does. Correlation measures the
//! *shape* around the mean, so a flat vector matches nothing.

use std::collections::VecDeque;
use std::f32::consts::PI;
use std::time::{Duration, Instant};

use super::{SAMPLE_RATE, fft, interpolate};
use crate::audio_tap::AudioTap;

/// Samples analyzed, and the transform they are windowed into (~371 ms at
/// 44.1 kHz). Long by the standards of the other analyzers because harmony,
/// unlike a transient, is only visible over time: two semitones at the bottom
/// of the analyzed span stand 6.5 Hz apart, and a Hann window separates them
/// only over a window about this long. This is what sets [`RING_CAP`].
///
/// [`RING_CAP`]: crate::audio_tap
const WINDOW: usize = 16384;
const FFT_N: usize = 16384;

/// Lowest semitone analyzed, as a MIDI note number, and how many follow it:
/// A2 (110 Hz) up through G#6, four octaves.
///
/// The bottom is where the transform stops separating semitones. The top is
/// where a note's own harmonics start outrunning the analyzed span, which
/// leaves the salience below reading a fundamental against nothing.
const PITCH_LO: usize = 45;
const PITCHES: usize = 48;

/// How often the transform runs.
///
/// Deliberately slower than the draw loop. A chord holds for hundreds of
/// milliseconds, so transforming sixteen thousand samples on every 50 ms frame
/// is work for an answer that has not changed. The envelope below still
/// advances every frame, so the picture stays smooth.
const ANALYSIS: Duration = Duration::from_millis(100);

/// Points sampled across a semitone's slot. The peak of these is the note's
/// level, so a note reads whatever it is tuned to inside its own slot — a
/// record cut a few cents sharp still lands on its own pitch.
const PROBES: usize = 5;
/// Half a semitone, as a frequency ratio. The edges of that slot.
const SEMITONE_HALF: f32 = 1.029_302_2;

/// Weights the first four harmonics are summed with, fundamental first.
///
/// This is the step that makes the whole thing work. A single played note puts
/// energy on its octave, its twelfth and its double octave as well as on
/// itself, and two of those land on pitch classes the chord does not contain.
/// Gathering a note's whole series onto the note reinforces fundamentals,
/// while a stray harmonic keeps only the one peak it stands on.
const HARMONIC_WEIGHTS: [f32; 4] = [1.0, 0.5, 0.33, 0.25];

/// Expands the bottom of the chroma so the pitch classes a chord does not use
/// fall away instead of hovering. The same reasoning as [`super::spectrum::CONTRAST`],
/// with one more job here: it is also what separates a chord's shape from a
/// drum break's, which the gates below read.
const CHROMA_CONTRAST: f32 = 2.0;
/// How fast the chroma follows the analysis, rising and falling.
///
/// Asymmetric, the way [`super::Pulse`]'s envelope is: a note that starts
/// should be on the field about when it is audible, while one that stops has
/// to leave slowly enough to ride out the gap between two strums of the same
/// chord. A single time constant either lags every change or lets the columns
/// chatter under a melody.
const CHROMA_ATTACK: f32 = 0.12;
const CHROMA_RELEASE: f32 = 0.35;
/// Below this a pitch class is treated as fully at rest, so idle frames settle
/// to exactly zero rather than decaying forever.
const REST: f32 = 0.004;
/// Below this the window is treated as silence, so a muted stream names no
/// chord instead of naming whatever shape its noise floor happens to have.
const SILENCE: f32 = 1e-3;

/// How far the chroma must stand off its own mean before it is read as
/// harmony at all.
///
/// A record playing chords puts most of its energy on three or four pitch
/// classes; a drum break puts it on all twelve. Without this gate the matcher
/// still returns whichever chord a noise floor's fluctuations happen to
/// resemble, and the field names chords through a passage that has none.
const PEAK_FLOOR: f32 = 0.5;
/// How well the winning chord has to fit before it is named.
const MATCH_FLOOR: f32 = 0.6;
/// How much better a rival has to be before the held chord gives way, and the
/// shortest time a chord is held for.
///
/// Both are needed. Relative pairs like `Am` and `C` share two of three notes,
/// so their scores cross whenever the melody moves; the margin stops the name
/// flickering between them, and the hold stops a single analysis doing it.
const SWITCH_MARGIN: f32 = 1.06;
const MIN_HOLD: Duration = Duration::from_millis(250);

/// How long a chord has to stand before the history strip records it.
///
/// A change crosses from one chroma to the other rather than cutting, and part
/// way across the pair can genuinely look like a third chord — `C` moving to
/// `G` passes through `Gsus4`. The name row shows that reading because it is
/// what is being heard at that instant, but the strip is a record of the
/// progression, and a progression is the chords that stood.
const HISTORY_HOLD: Duration = Duration::from_millis(400);

/// Preference for the plainer name on a near-tie. A seventh has to be
/// genuinely sounding to be named, rather than winning on a chord tone that
/// happened to be under the melody.
const SEVENTH_BIAS: f32 = 0.97;

/// Chords kept for the field's history strip.
const HISTORY_CAP: usize = 16;

/// The twelve pitch classes, in the order the chroma holds them.
pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// What a chord is built of, above its root.
///
/// The order is the matcher's tie-break order, so the plainest reading of an
/// ambiguous chroma is the one that gets named.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Major,
    Minor,
    Dominant7,
    Minor7,
    Major7,
    Sus4,
    Diminished,
    Augmented,
}

impl Quality {
    const ALL: [Self; 8] = [
        Self::Major,
        Self::Minor,
        Self::Dominant7,
        Self::Minor7,
        Self::Major7,
        Self::Sus4,
        Self::Diminished,
        Self::Augmented,
    ];

    /// Semitones above the root.
    fn tones(self) -> &'static [u8] {
        match self {
            Self::Major => &[0, 4, 7],
            Self::Minor => &[0, 3, 7],
            Self::Dominant7 => &[0, 4, 7, 10],
            Self::Minor7 => &[0, 3, 7, 10],
            Self::Major7 => &[0, 4, 7, 11],
            Self::Sus4 => &[0, 5, 7],
            Self::Diminished => &[0, 3, 6],
            Self::Augmented => &[0, 4, 8],
        }
    }

    /// What follows the root in the chord's name.
    fn suffix(self) -> &'static str {
        match self {
            Self::Major => "",
            Self::Minor => "m",
            Self::Dominant7 => "7",
            Self::Minor7 => "m7",
            Self::Major7 => "maj7",
            Self::Sus4 => "sus4",
            Self::Diminished => "dim",
            Self::Augmented => "aug",
        }
    }

    fn bias(self) -> f32 {
        match self.tones().len() {
            4 => SEVENTH_BIAS,
            _ => 1.0,
        }
    }
}

/// A named chord: a pitch class and what stands on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Chord {
    pub root: u8,
    pub quality: Quality,
}

impl Chord {
    pub fn name(self) -> String {
        format!(
            "{}{}",
            NOTE_NAMES[self.root as usize % 12],
            self.quality.suffix()
        )
    }

    /// The pitch classes this chord contains, one bit each, so the renderer can
    /// light a column without rebuilding the template.
    pub fn mask(self) -> u16 {
        self.quality
            .tones()
            .iter()
            .fold(0, |mask, tone| mask | 1 << ((self.root + tone) % 12))
    }

    fn template(self) -> [f32; 12] {
        let mut template = [0.0; 12];
        for tone in self.quality.tones() {
            template[((self.root + tone) % 12) as usize] = 1.0;
        }
        template
    }
}

/// The rolling chroma, the chord standing on it, and what came before.
pub struct Chords {
    chroma: [f32; 12],
    /// What the last analysis asked the chroma to become.
    target: [f32; 12],
    held: Option<Chord>,
    /// When [`Self::held`] was adopted, which [`MIN_HOLD`] and
    /// [`HISTORY_HOLD`] are both measured from.
    held_since: Option<Instant>,
    /// Whether the strip already carries what is held, so a chord that stands
    /// for a while is recorded once rather than on every frame after it
    /// qualifies.
    recorded: bool,
    confidence: f32,
    history: VecDeque<Chord>,
    /// Wall clock of the previous update; the chroma follows real time because
    /// the draw loop also redraws on input events, not just on the tick.
    last_update: Option<Instant>,
    last_analysis: Option<Instant>,
    samples: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    mag: Vec<f32>,
}

impl Chords {
    pub fn new() -> Self {
        Self {
            chroma: [0.0; 12],
            target: [0.0; 12],
            held: None,
            held_since: None,
            recorded: false,
            confidence: 0.0,
            history: VecDeque::with_capacity(HISTORY_CAP),
            last_update: None,
            last_analysis: None,
            samples: Vec::new(),
            re: vec![0.0; FFT_N],
            im: vec![0.0; FFT_N],
            mag: vec![0.0; FFT_N / 2 + 1],
        }
    }

    /// How much of each pitch class is sounding, in 0..=1 against the loudest.
    pub fn chroma(&self) -> &[f32; 12] {
        &self.chroma
    }

    /// The chord being held, or [`None`] through a passage with no harmony in
    /// it.
    pub fn current(&self) -> Option<Chord> {
        self.held
    }

    /// How well the held chord fits, in 0..=1. The field reads it as
    /// brightness.
    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    /// The chords gone through, oldest first, ending on the one held now.
    pub fn history(&self) -> &VecDeque<Chord> {
        &self.history
    }

    /// Analyze the tap and advance the chroma to `now`.
    ///
    /// A stale tap (`fresh == false`) means paused, buffering, or audio coming
    /// from another device: the chroma falls to rest, and the name goes with it
    /// once the hold expires.
    pub fn update(&mut self, tap: &AudioTap, fresh: bool, now: Instant) {
        // The same clamp the other analyzers use: the first frame has no
        // reference point, and a long stall must not advance the envelope by a
        // whole period in one step.
        let dt = match self.last_update {
            Some(prev) => now.saturating_duration_since(prev).as_secs_f32().min(0.5),
            None => 0.05,
        };
        self.last_update = Some(now);

        let due = self
            .last_analysis
            .is_none_or(|at| now.saturating_duration_since(at) >= ANALYSIS);
        if !fresh {
            // Staleness is read every frame rather than every analysis, so a
            // pause reaches the picture at once.
            self.target = [0.0; 12];
            self.samples.clear();
        } else if due {
            tap.latest(&mut self.samples, WINDOW);
            // A ring this far from full is the first frames of a track, which
            // is "not yet" rather than silence.
            if self.samples.len() >= WINDOW / 2 {
                self.analyze();
            } else {
                self.target = [0.0; 12];
            }
        }
        if due {
            self.last_analysis = Some(now);
        }

        let (rise, fall) = (
            1.0 - (-dt / CHROMA_ATTACK).exp(),
            1.0 - (-dt / CHROMA_RELEASE).exp(),
        );
        for (level, target) in self.chroma.iter_mut().zip(self.target) {
            let k = if target > *level { rise } else { fall };
            *level += (target - *level) * k;
            if *level < REST {
                *level = 0.0;
            }
        }
        self.name_it(now);
    }

    /// Fill `self.target` with this window's chroma.
    fn analyze(&mut self) {
        let n = self.samples.len();

        // Hann window, then zero-pad to the transform's length. Without the
        // window a loud note leaks across the semitones either side of it and
        // the chroma reads as a smear.
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

        // Hann halves the coherent gain and only half the spectrum is kept, so
        // a full-scale sine lands near 1.0 and [`SILENCE`] can be an absolute
        // level.
        let scale = 4.0 / n as f32;
        for (k, m) in self.mag.iter_mut().enumerate() {
            *m = (self.re[k] * self.re[k] + self.im[k] * self.im[k]).sqrt() * scale;
        }

        let bin_hz = SAMPLE_RATE / FFT_N as f32;
        let mut chroma = [0.0f32; 12];
        for pitch in PITCH_LO..PITCH_LO + PITCHES {
            let root = pitch_hz(pitch);
            let salience: f32 = HARMONIC_WEIGHTS
                .iter()
                .enumerate()
                .map(|(h, w)| w * peak_near(&self.mag, root * (h + 1) as f32, bin_hz))
                .sum();
            chroma[pitch % 12] += salience;
        }

        let loudest = chroma.iter().copied().fold(0.0f32, f32::max);
        if loudest <= SILENCE {
            self.target = [0.0; 12];
            return;
        }
        for (target, level) in self.target.iter_mut().zip(chroma) {
            *target = (level / loudest).powf(CHROMA_CONTRAST);
        }
    }

    /// Score every chord against the chroma and decide what the field says.
    fn name_it(&mut self, now: Instant) {
        let candidate = self.candidate();
        let settled = self
            .held_since
            .is_none_or(|at| now.saturating_duration_since(at) >= MIN_HOLD);

        let Some((chord, score)) = candidate else {
            // One bad analysis must not blank a chord that is still sounding,
            // so the rest waits for the hold to run out.
            if settled {
                self.held = None;
                self.held_since = None;
                self.confidence = 0.0;
            }
            return;
        };

        self.confidence = score;
        let keep = match self.held {
            Some(held) if held == chord => true,
            // A rival has to beat what is held by a margin, not merely tie it.
            Some(held) => !settled || score <= correlate(&self.chroma, held) * SWITCH_MARGIN,
            None => false,
        };
        if !keep {
            self.held = Some(chord);
            self.held_since = Some(now);
            self.recorded = false;
        }
        self.record(now);
    }

    /// Put the held chord on the strip, once it has stood long enough to be
    /// part of the progression rather than part of a change.
    fn record(&mut self, now: Instant) {
        let stood = self
            .held_since
            .is_some_and(|at| now.saturating_duration_since(at) >= HISTORY_HOLD);
        let Some(chord) = self.held.filter(|_| stood && !self.recorded) else {
            return;
        };
        self.recorded = true;
        if self.history.back() == Some(&chord) {
            return;
        }
        if self.history.len() == HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(chord);
    }

    /// The best-fitting chord, once the chroma has cleared both gates.
    fn candidate(&self) -> Option<(Chord, f32)> {
        if peakiness(&self.chroma) < PEAK_FLOOR {
            return None;
        }
        let best = (0..12u8)
            .flat_map(|root| Quality::ALL.map(|quality| Chord { root, quality }))
            .map(|chord| (chord, correlate(&self.chroma, chord)))
            .reduce(|best, next| if next.1 > best.1 { next } else { best })?;
        (best.1 >= MATCH_FLOOR).then_some(best)
    }
}

impl Default for Chords {
    fn default() -> Self {
        Self::new()
    }
}

fn pitch_hz(midi: usize) -> f32 {
    440.0 * ((midi as f32 - 69.0) / 12.0).exp2()
}

/// The loudest point within half a semitone of `hz`.
///
/// Peak rather than a sum over the slot: a slot widens with frequency, so a
/// sum reads broadband content as a rising tone and the top octave drowns the
/// bottom one. A harmonic that runs past the analyzed spectrum contributes
/// nothing rather than the top bin's level.
fn peak_near(mag: &[f32], hz: f32, bin_hz: f32) -> f32 {
    let (lo, hi) = (hz / SEMITONE_HALF, hz * SEMITONE_HALF);
    if hi / bin_hz >= (mag.len() - 1) as f32 {
        return 0.0;
    }
    let span = hi / lo;
    (0..PROBES)
        .map(|probe| {
            let f = lo * span.powf(probe as f32 / (PROBES - 1) as f32);
            interpolate(mag, f / bin_hz)
        })
        .fold(0.0f32, f32::max)
}

/// How far the chroma stands off its own mean, in 0..=1.
///
/// Zero on a vector with the same level in every pitch class, which is what a
/// drum break and a noise floor both look like.
fn peakiness(chroma: &[f32; 12]) -> f32 {
    let loudest = chroma.iter().copied().fold(0.0f32, f32::max);
    if loudest <= 0.0 {
        return 0.0;
    }
    1.0 - mean(chroma) / loudest
}

fn mean(v: &[f32; 12]) -> f32 {
    v.iter().sum::<f32>() / v.len() as f32
}

/// How well the chroma's shape fits `chord`, in -1..=1.
///
/// Both vectors are measured against their own means, so the answer is about
/// shape alone: a flat chroma correlates with nothing, where a plain overlap
/// would score it against every chord in the dictionary at once.
fn correlate(chroma: &[f32; 12], chord: Chord) -> f32 {
    let template = chord.template();
    let (mc, mt) = (mean(chroma), mean(&template));
    let (mut dot, mut norm_c, mut norm_t) = (0.0f32, 0.0f32, 0.0f32);
    for (c, t) in chroma.iter().zip(&template) {
        let (c, t) = (c - mc, t - mt);
        dot += c * t;
        norm_c += c * c;
        norm_t += t * t;
    }
    if norm_c <= f32::EPSILON || norm_t <= f32::EPSILON {
        return 0.0;
    }
    dot / (norm_c * norm_t).sqrt() * chord.quality.bias()
}

/// Harmonics a fixture note is built from, at `1 / h` amplitude.
///
/// One more than the salience above gathers, and deliberately so: the fifth
/// harmonic is the major third, and a fixture without it would let a detector
/// that reads bare spectral peaks pass every test here.
#[cfg(test)]
const FIXTURE_HARMONICS: usize = 5;

/// A chord as an instrument would sound it, each note at its own level, at an
/// overall level a master sits at. The renderer's own tests need one of these
/// as much as this module's do.
#[cfg(test)]
pub fn voiced(midi: &[(usize, f32)]) -> Vec<f32> {
    (0..WINDOW)
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE;
            midi.iter()
                .flat_map(|&(note, amp)| {
                    (1..=FIXTURE_HARMONICS).map(move |h| {
                        let f = pitch_hz(note) * h as f32;
                        amp * (2.0 * PI * f * t).sin() / h as f32
                    })
                })
                .sum::<f32>()
                * 0.08
        })
        .collect()
}

/// The same chord with every note sounded equally.
#[cfg(test)]
pub fn chord_signal(midi: &[usize]) -> Vec<f32> {
    voiced(&midi.iter().map(|&note| (note, 1.0)).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::super::pink_noise;
    use super::chord_signal as notes;
    use super::*;

    fn tap_of(signal: &[f32]) -> AudioTap {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = signal.iter().flat_map(|&s| [s as f64, s as f64]).collect();
        tap.push(&stereo, 1.0);
        tap
    }

    /// Run the analyzer over a tap for `secs` of simulated time at the fast
    /// tick's rate, starting at `from`.
    fn feed(chords: &mut Chords, tap: &AudioTap, from: Instant, secs: u64) -> Instant {
        let frames = secs * 20;
        for frame in 1..=frames {
            chords.update(tap, true, from + Duration::from_millis(50 * frame));
        }
        from + Duration::from_millis(50 * frames)
    }

    fn heard(signal: &[f32]) -> Option<Chord> {
        let mut chords = Chords::new();
        feed(&mut chords, &tap_of(signal), Instant::now(), 3);
        chords.current()
    }

    fn named(signal: &[f32]) -> String {
        heard(signal).map_or_else(|| "-".to_string(), Chord::name)
    }

    #[test]
    fn a_major_triad_reads_as_major() {
        assert_eq!(named(&notes(&[60, 64, 67])), "C");
    }

    /// The test the harmonic salience exists for. A minor third is the pitch
    /// class a major third's own fifth harmonic lands on, so a detector reading
    /// bare spectral peaks calls every minor chord major.
    #[test]
    fn a_minor_triad_is_not_called_major() {
        assert_eq!(named(&notes(&[57, 60, 64])), "Am");
    }

    #[test]
    fn a_dominant_seventh_keeps_its_seventh() {
        assert_eq!(named(&notes(&[55, 59, 62, 65])), "G7");
    }

    #[test]
    fn a_minor_seventh_keeps_both_its_thirds() {
        assert_eq!(named(&notes(&[57, 60, 64, 67])), "Am7");
    }

    /// A chord is a shape, so the same shape has to name the same quality
    /// wherever it starts.
    #[test]
    fn transposing_a_triad_transposes_its_name() {
        for root in 0..12usize {
            let base = 57 + root;
            let expected = format!("{}m", NOTE_NAMES[base % 12]);
            assert_eq!(
                named(&notes(&[base, base + 3, base + 7])),
                expected,
                "the minor triad on {expected} did not transpose"
            );
        }
    }

    /// A drum break has energy in every pitch class. It has to read as no
    /// chord rather than as whichever one the noise floor resembles.
    #[test]
    fn noise_reads_as_no_chord() {
        assert_eq!(heard(&pink_noise(WINDOW)), None);
    }

    #[test]
    fn silence_reads_as_no_chord() {
        assert_eq!(heard(&vec![0.0; WINDOW]), None);
    }

    /// Relative pairs share two of three notes, so a melody note is enough to
    /// cross their scores. The name has to sit still through that: C major
    /// under a sixth is still C major, not A minor's seventh.
    #[test]
    fn a_held_chord_survives_a_passing_note() {
        let mut chords = Chords::new();
        let t = feed(
            &mut chords,
            &tap_of(&notes(&[60, 64, 67])),
            Instant::now(),
            3,
        );
        assert_eq!(chords.current().map(Chord::name).as_deref(), Some("C"));

        let melody = tap_of(&voiced(&[(60, 1.0), (64, 1.0), (67, 1.0), (69, 0.45)]));
        feed(&mut chords, &melody, t, 2);
        assert_eq!(chords.current().map(Chord::name).as_deref(), Some("C"));
    }

    #[test]
    fn the_history_records_a_change() {
        let mut chords = Chords::new();
        let t = feed(
            &mut chords,
            &tap_of(&notes(&[60, 64, 67])),
            Instant::now(),
            3,
        );
        feed(&mut chords, &tap_of(&notes(&[55, 59, 62])), t, 3);
        let names: Vec<String> = chords.history().iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["C".to_string(), "G".to_string()]);
    }

    /// A stale tap is paused, buffering, or playing on another device.
    /// Whichever it is, the chroma falls from where it was rather than
    /// snapping, so a single missed frame does not blank the field.
    #[test]
    fn a_stale_tap_falls_rather_than_snapping() {
        let mut chords = Chords::new();
        let tap = tap_of(&notes(&[60, 64, 67]));
        let t = feed(&mut chords, &tap, Instant::now(), 3);
        let lit = *chords
            .chroma()
            .iter()
            .max_by(|a, b| a.total_cmp(b))
            .unwrap();
        assert!(lit > 0.9, "{lit}");

        let next = {
            chords.update(&tap, false, t + Duration::from_millis(50));
            *chords
                .chroma()
                .iter()
                .max_by(|a, b| a.total_cmp(b))
                .unwrap()
        };
        assert!(next < lit && next > 0.5, "snapped to {next} from {lit}");
        assert!(
            chords.current().is_some(),
            "one stale frame dropped the name"
        );

        for frame in 2..=40u64 {
            chords.update(&tap, false, t + Duration::from_millis(50 * frame));
        }
        assert!(chords.chroma().iter().all(|&c| c == 0.0));
        assert_eq!(chords.current(), None);
    }

    /// The chroma is driven by elapsed time, not by frame count, because the
    /// loop redraws on input as well as on the tick.
    #[test]
    fn the_chroma_is_time_based_not_frame_based() {
        let tap = tap_of(&notes(&[60, 64, 67]));
        let settle = |step_ms: u64, frames: u64| {
            let mut chords = Chords::new();
            let t0 = Instant::now();
            for frame in 1..=frames {
                chords.update(&tap, true, t0 + Duration::from_millis(step_ms * frame));
            }
            *chords
                .chroma()
                .iter()
                .max_by(|a, b| a.total_cmp(b))
                .unwrap()
        };
        let fast = settle(50, 20);
        let slow = settle(250, 4);
        assert!((fast - slow).abs() < 0.05, "fast {fast}, slow {slow}");
    }

    #[test]
    fn the_history_does_not_grow_past_its_cap() {
        let mut chords = Chords::new();
        let mut t = Instant::now();
        for root in 0..20usize {
            let base = 57 + root % 12;
            t = feed(
                &mut chords,
                &tap_of(&notes(&[base, base + 4, base + 7])),
                t,
                2,
            );
        }
        assert_eq!(chords.history().len(), HISTORY_CAP);
    }
}
