use std::sync::atomic::{AtomicU32, Ordering};

use ratatui::style::{Color, Style};

use crate::cover::Cover;

/// The chrome palette is truecolor, so the app looks the same whatever the
/// terminal's 16-colour scheme is.
///
/// The resting accent is a gold that sits between [`FLAME`]'s gold and orange
/// stops, so the UI accent is a colour the visualizer already paints rather
/// than a fourth hue on the screen. The grey ramp is cooled slightly against
/// it: a neutral grey next to this much warmth reads as blue-ish, and a warm
/// grey would muddy into the accent.
pub const ACCENT: Color = Color::Rgb(0xE8, 0xA0, 0x2E);
pub const TEXT: Color = Color::Rgb(0xAA, 0xB2, 0xB6);
pub const DIM: Color = Color::Rgb(0x58, 0x61, 0x66);
/// Search box and toasts. A pale, desaturated yellow: against a gold
/// [`ACCENT`] a gold border would stop reading as a distinct state.
pub const WARN: Color = Color::Rgb(0xF0, 0xDC, 0x8A);
/// Emphasis for the selected row and the playing track's own row. Selection is
/// carried by weight and brightness rather than a filled bar, so it never
/// competes with the accent-colored "now playing" marker.
pub const BRIGHT: Color = Color::Rgb(0xE8, 0xF0, 0xF2);
/// The search field's resting fill, and the only region of the app that is
/// painted rather than left transparent to the terminal's own ground. A fill
/// says "field" without a border, which this UI declines everywhere else (see
/// [`super::table::draw_scrollbar`]).
///
/// Sits between the terminal's ground and [`DIM`]: dark enough that the row
/// does not read as a highlight or a selection, light enough that the field
/// has an edge on a dark terminal.
pub const FIELD: Color = Color::Rgb(0x22, 0x27, 0x2B);
/// The same field under the pointer. Brighter by about as much again, so the
/// field lights the way every other control does without jumping to the flat
/// [`DIM`] pill that [`super::table::hover_style`] paints — that pill is sized
/// to a word, and the same value across a whole row would read as a selection.
pub const FIELD_HOVER: Color = Color::Rgb(0x31, 0x38, 0x3D);
/// The green the header's status dot pulses in while audio is running.
const GREEN_RGB: (u8, u8, u8) = (0x5F, 0xBF, 0x62);
pub const GREEN: Color = Color::Rgb(GREEN_RGB.0, GREEN_RGB.1, GREEN_RGB.2);

/// Visualizer flame ramp, keyed on height alone so every bar shares the same
/// green / yellow / red banding and the field reads as one LED meter. The
/// stops are spaced to hold each color over a stretch rather than sweep
/// continuously — mixing hues per bar just reads as noise.
///
/// This is the fallback: a sleeve with two usable hues drives the ramp instead
/// (see [`set_cover_colors`]), and this stands only when it has none.
///
/// Brighter than a solid-block palette would need to be: bars are drawn with
/// half-height `▄` cells, so only half the pixels of each cell are lit.
///
/// The stops run green, green, chartreuse, gold, orange, red from the resting
/// bed to the tips.
const FLAME: Stops = [
    (0.00, (0x35, 0xA5, 0x50)),
    (0.46, (0x6F, 0xDE, 0x45)),
    (0.58, (0xD6, 0xE8, 0x33)),
    (0.74, (0xFF, 0xC6, 0x2E)),
    (0.88, (0xFF, 0x8A, 0x22)),
    (1.00, (0xF5, 0x43, 0x28)),
];

/// A visualizer ramp: six colors at fixed positions up the field.
type Stops = [(f32, (u8, u8, u8)); 6];

/// Value assigned to each stop of a cover-derived ramp, in the same order as
/// [`FLAME`]. The brightnesses are ours rather than the sleeve's, so the ramp
/// climbs whatever pair of hues it is handed — the accent can afford to take
/// the cover's own value (see [`accent_color`]) because it is one flat color,
/// but a gradient that does not brighten upward stops reading as a meter.
///
/// The span from floor to tip is wide because it is the *only* thing carrying
/// a single-hue sleeve's ramp: an all-red cover travels in brightness and
/// saturation alone, and a narrow band of reds would read as one flat colour.
const RAMP_VALUES: [f32; 6] = [0.34, 0.72, 0.79, 0.86, 1.00, 1.00];
/// How far the top stop is pulled toward white, for tips that read as hot.
const TIP_LIGHTEN: f32 = 0.45;
/// The second hue is desaturated a little as it brightens, so the two upper
/// stops separate by more than value alone.
const HIGH_SAT: f32 = 0.8;

/// How brightly one visualizer LED is lit.
///
/// Every cell is drawn with the same half-height `▄`, so brightness is the
/// only thing that varies. A shading glyph would be the obvious way to draw a
/// partly-lit cell, but `░▒▓` fill the *whole* character cell while `▄` fills
/// half of it — a dithered cell then visibly hangs above the LED row it
/// belongs to.
pub enum Led {
    /// Fully lit: the bar reaches past this row.
    Lit,
    /// The bar's top row, only partly into this LED.
    Half,
    /// Afterglow: the bar was here recently and is falling away.
    Trail,
}

/// Color for one visualizer LED. `pos` is 0 at the pane floor, 1 at its top.
pub fn viz_color(pos: f32, led: Led) -> Color {
    let (r, g, b) = ramp_color(&stops(), pos);
    scale(
        r,
        g,
        b,
        match led {
            Led::Lit => 1.0,
            Led::Half => 0.55,
            Led::Trail => 0.35,
        },
    )
}

fn ramp_color(stops: &Stops, pos: f32) -> (u8, u8, u8) {
    let t = pos.clamp(0.0, 1.0);
    let hi = stops
        .iter()
        .position(|&(stop, _)| t <= stop)
        .unwrap_or(1)
        .max(1);
    let (t0, (r0, g0, b0)) = stops[hi - 1];
    let (t1, (r1, g1, b1)) = stops[hi];
    let f = (t - t0) / (t1 - t0);
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f).round() as u8;
    (lerp(r0, r1), lerp(g0, g1), lerp(b0, b1))
}

/// The ramp in force right now: the playing cover's two hues when it has them,
/// otherwise [`FLAME`].
fn stops() -> Stops {
    match viz_pair() {
        Some(pair) => cover_stops(pair),
        None => FLAME,
    }
}

/// Six stops from a sleeve's hues.
///
/// Only the hues survive; the positions are [`FLAME`]'s, so the banding stays,
/// and the saturations and values are ours, so the field always brightens
/// toward its tips. The middle stop is the crossover, where the lower hue
/// hands off to the upper one.
///
/// The darker of the two hues takes the floor — compared at a *common* value,
/// so this ranks the hues themselves rather than however bright they happened
/// to be on the sleeve. Since brightness within a hue is then set by
/// [`RAMP_VALUES`], the finished ramp climbs whichever pair it is handed; a
/// yellow floor under a blue tip would otherwise read upside down.
///
/// The pair may be the same hue twice, for a sleeve that only has one (see
/// [`crate::cover::palette`]). Nothing special is needed for that: the ramp
/// simply travels in brightness and saturation instead of hue, which on a red
/// cover is a deep red climbing to a pale one.
fn cover_stops(pair: [[u8; 3]; 2]) -> Stops {
    let hue = |rgb: [u8; 3]| {
        let (h, s, _) = crate::cover::to_hsv(rgb);
        (h, s.max(crate::cover::ACCENT_SAT))
    };
    let (mut lo, mut hi) = (hue(pair[0]), hue(pair[1]));
    let at = |(h, s): (f32, f32), i: usize| crate::cover::from_hsv(h, s, RAMP_VALUES[i]);
    if luminance(at(lo, 4)) > luminance(at(hi, 4)) {
        std::mem::swap(&mut lo, &mut hi);
    }
    let tip = (hi.0, hi.1 * HIGH_SAT);

    let mut out = FLAME;
    let colors = [
        at(lo, 0),
        at(lo, 1),
        // The crossover, where the floor hue hands off to the tip hue.
        at((mid_hue(lo.0, hi.0), (lo.1 + hi.1) / 2.0), 2),
        at(hi, 3),
        at(tip, 4),
        lighten(at(tip, 5), TIP_LIGHTEN),
    ];
    for (stop, [r, g, b]) in out.iter_mut().zip(colors) {
        stop.1 = (r, g, b);
    }
    out
}

/// The crossover hue, half way from `lo` to `hi` around the colour circle.
///
/// It has to be found in hue rather than by blending the two colours in RGB: a
/// blend of hues that sit opposite each other passes through grey, which puts a
/// dead band across the middle of the field.
///
/// Two hues can be joined either way round, and for a pair that sits nearly
/// opposite there is no "shorter way" worth the name — so pick by brightness
/// instead, taking the midpoint whose own luminance sits nearest between the
/// two endpoints'. Blue to yellow then crosses at magenta rather than at a
/// green brighter than the tips it is meant to be leading up to. Identical
/// hues give that hue back, so a single-hue sleeve has no crossover to make.
fn mid_hue(lo: f32, hi: f32) -> f32 {
    let half = ((hi - lo + 540.0) % 360.0 - 180.0) / 2.0;
    let target = (hue_luminance(lo) + hue_luminance(hi)) / 2.0;
    [lo + half, lo + half + 180.0]
        .into_iter()
        .map(|h| (h + 360.0) % 360.0)
        .min_by(|&a, &b| {
            let miss = |h| (hue_luminance(h) - target).abs();
            miss(a).total_cmp(&miss(b))
        })
        .expect("two candidates")
}

/// How bright a hue is at its most vivid. Hues differ enormously here — a full
/// yellow is ten times the luminance of a full blue — which is why the ramp
/// ranks hues rather than trusting the values they arrived with.
fn hue_luminance(h: f32) -> f32 {
    luminance(crate::cover::from_hsv(h, 1.0, 1.0))
}

/// Rough perceived brightness, for ranking colours against each other.
fn luminance([r, g, b]: [u8; 3]) -> f32 {
    0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32
}

fn scale(r: u8, g: u8, b: u8, k: f32) -> Color {
    let ch = |v: u8| (v as f32 * k).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(ch(r), ch(g), ch(b))
}

/// Accent override installed from the playing album's cover, as packed
/// `0x00RRGGBB`, or [`NO_OVERRIDE`] when the static [`ACCENT`] applies.
///
/// An atomic rather than a parameter on [`accent`]: the override changes once
/// per track and is read from every one of the ~20 call sites across the UI
/// modules, so threading it through would be churn for nothing. The client
/// task writes it; the draw loop only ever reads.
static ACCENT_OVERRIDE: AtomicU32 = AtomicU32::new(NO_OVERRIDE);
/// The visualizer ramp's two hues, packed the same way. [`VIZ_A`] holding
/// [`NO_OVERRIDE`] means no cover ramp is installed and [`FLAME`] applies, so
/// the pair is written A-last and read A-first — a torn read then falls back
/// to the built-in ramp rather than mixing two covers' colours.
static VIZ_A: AtomicU32 = AtomicU32::new(NO_OVERRIDE);
static VIZ_B: AtomicU32 = AtomicU32::new(NO_OVERRIDE);
const NO_OVERRIDE: u32 = u32::MAX;

fn pack(rgb: Option<[u8; 3]>) -> u32 {
    match rgb {
        Some([r, g, b]) => u32::from_be_bytes([0, r, g, b]),
        None => NO_OVERRIDE,
    }
}

fn unpack(packed: u32) -> Option<[u8; 3]> {
    match packed {
        NO_OVERRIDE => None,
        packed => {
            let [_, r, g, b] = packed.to_be_bytes();
            Some([r, g, b])
        }
    }
}

/// Install the playing album's colours — the accent for the UI chrome, and the
/// two-hue ramp for the visualizer — or `None` to go back to the built-in
/// [`ACCENT`] and [`FLAME`].
///
/// One call rather than one per colour: they come off the same [`Cover`] and a
/// track change must move both, so a caller cannot update one and forget the
/// other.
pub fn set_cover_colors(cover: Option<&Cover>) {
    let ramp = cover.and_then(|c| c.ramp);
    VIZ_B.store(pack(ramp.map(|r| r[1])), Ordering::Relaxed);
    VIZ_A.store(pack(ramp.map(|r| r[0])), Ordering::Relaxed);
    ACCENT_OVERRIDE.store(pack(cover.and_then(|c| c.accent)), Ordering::Relaxed);
}

/// The cover ramp's two hues, or `None` when [`FLAME`] applies. Unordered —
/// [`cover_stops`] decides which one is the floor.
fn viz_pair() -> Option<[[u8; 3]; 2]> {
    let a = unpack(VIZ_A.load(Ordering::Relaxed))?;
    let b = unpack(VIZ_B.load(Ordering::Relaxed))?;
    Some([a, b])
}

/// The accent in force right now: the cover's colour when one is installed,
/// otherwise the static [`ACCENT`].
///
/// The colour arrives already lifted to a legible saturation and brightness
/// (see [`crate::cover::dominant`]), so there is no contrast guard here — a
/// dark sleeve yields a bright accent rather than an unreadable one. The
/// visualizer ramp does guard itself, because it has to climb rather than just
/// be readable; see [`cover_stops`].
pub fn accent_color() -> Color {
    match unpack(ACCENT_OVERRIDE.load(Ordering::Relaxed)) {
        Some([r, g, b]) => Color::Rgb(r, g, b),
        None => ACCENT,
    }
}

pub fn accent() -> Style {
    Style::default().fg(accent_color())
}

/// How far [`accent_bright`] pulls the accent toward white.
const LIGHTEN: f32 = 0.4;

/// Blend `rgb` toward white by `k`.
fn lighten([r, g, b]: [u8; 3], k: f32) -> [u8; 3] {
    let ch = |v: u8| (v as f32 + (255.0 - v as f32) * k).round() as u8;
    [ch(r), ch(g), ch(b)]
}

/// The hover accent: whatever [`accent_color`] currently is, pulled toward
/// white. Derived rather than a second constant so a cover-driven accent
/// brightens on hover the same way the built-in one does.
pub fn accent_bright() -> Color {
    match accent_color() {
        Color::Rgb(r, g, b) => {
            let [r, g, b] = lighten([r, g, b], LIGHTEN);
            Color::Rgb(r, g, b)
        }
        other => other,
    }
}

pub fn text() -> Style {
    Style::default().fg(TEXT)
}

pub fn dim() -> Style {
    Style::default().fg(DIM)
}

pub fn bright() -> Style {
    Style::default().fg(BRIGHT)
}

/// [`WARN`] as a style: something is in flight. The toast and the search
/// prompt built this inline; the nav row's `● LOADING` is the third site, and
/// three is where the colour stops being a one-off.
pub fn warn() -> Style {
    Style::default().fg(WARN)
}

pub fn green() -> Style {
    Style::default().fg(GREEN)
}

/// The search field's ground, or its lit ground when the pointer is in it.
/// A background alone: the runs drawn over it bring their own foregrounds.
pub fn field(hover: bool) -> Style {
    Style::default().bg(if hover { FIELD_HOVER } else { FIELD })
}

/// The accent in force right now at a fraction of full brightness, for the
/// header's pulsing dot and the stopped transport. Derived from
/// [`accent_color`] rather than a fixed hue, so both move with whatever colour
/// the playing sleeve has put on the rest of the screen.
pub fn accent_at(k: f32) -> Style {
    match accent_color() {
        Color::Rgb(r, g, b) => Style::default().fg(scale(r, g, b, k)),
        other => Style::default().fg(other),
    }
}

/// How far [`stopped_dim`] pulls the accent back.
const PAUSED_FADE: f32 = 0.62;

/// The stopped transport's colour: the accent in force, held back. The
/// running state takes the accent at full strength; stopped is a resting
/// state and should sit back from the title beside it. Taking it from
/// [`accent_color`] rather than a fixed red keeps the transport in whatever
/// colour the playing sleeve has put on the rest of the screen.
pub fn stopped_dim() -> Style {
    accent_at(PAUSED_FADE)
}

/// Gradient pairs the cover-art placeholder picks from, keyed on the album
/// id so a given record always gets the same swatch.
///
/// Muted and dark on purpose: the block is a stand-in for artwork, not a
/// feature, and the `♫` drawn over it has to stay readable on every pair.
pub const PLACEHOLDER: [([u8; 3], [u8; 3]); 6] = [
    ([0x1E, 0x2A, 0x38], [0x33, 0x46, 0x52]),
    ([0x2B, 0x1F, 0x35], [0x46, 0x33, 0x52]),
    ([0x1C, 0x30, 0x2A], [0x2F, 0x4C, 0x40]),
    ([0x35, 0x26, 0x1C], [0x52, 0x3E, 0x2C]),
    ([0x22, 0x25, 0x3A], [0x39, 0x3D, 0x59]),
    ([0x33, 0x1E, 0x26], [0x4F, 0x33, 0x3D]),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The overrides are process-global, so nothing here installs one — every
    /// test drives [`cover_stops`] directly and leaves the atomics alone.
    /// Otherwise a test would recolour the player's field out from under the
    /// render tests running beside it.
    const BLUE: [u8; 3] = [20, 30, 120];
    const YELLOW: [u8; 3] = [230, 220, 90];
    const RED: [u8; 3] = [180, 30, 30];

    fn lum((r, g, b): (u8, u8, u8)) -> f32 {
        luminance([r, g, b])
    }

    /// The whole point of the assigned values: whichever way round the sleeve
    /// hands its hues over, the field brightens from floor to tip.
    #[test]
    fn a_cover_ramp_climbs_whichever_way_the_hues_arrive() {
        // Near-antipodal, adjacent, and the same hue twice.
        let pairs = [
            [BLUE, YELLOW],
            [YELLOW, BLUE],
            [RED, RED],
            [BLUE, RED],
            [[200, 40, 200], [40, 200, 200]],
            [[230, 120, 20], [120, 40, 160]],
        ];
        for pair in pairs {
            let stops = cover_stops(pair);
            // Floor to tip is the climb that has to hold; the crossover in
            // between is a hue journey and may dip, the way FLAME's own does.
            assert!(
                lum(stops[5].1) > 3.0 * lum(stops[0].1),
                "{pair:?}: {:?} to {:?} is not a climb",
                stops[0].1,
                stops[5].1
            );
            // And every stop stays a colour — a blend through grey would put a
            // dead band across the middle of the field.
            for &(pos, (r, g, b)) in &stops {
                let (_, s, _) = crate::cover::to_hsv([r, g, b]);
                assert!(s > 0.25, "{pair:?}: {pos} is washed out at {:?}", (r, g, b));
            }
        }
        // Yellow is the brighter hue, so it takes the tips either way round.
        for pair in [[BLUE, YELLOW], [YELLOW, BLUE]] {
            let (r, g, b) = cover_stops(pair)[5].1;
            assert!(
                r > b && g > b,
                "the tips are not the yellow: {:?}",
                (r, g, b)
            );
        }
    }

    /// A sleeve with one hue travels in brightness and saturation instead, so
    /// a red record still lights a red meter — deep at the floor, pale at the
    /// tips, red the whole way up.
    #[test]
    fn a_single_hue_cover_ramps_in_brightness_alone() {
        let stops = cover_stops([RED, RED]);
        for &(_, (r, g, b)) in &stops {
            assert!(r > g && r > b, "{:?} is not red", (r, g, b));
            assert_eq!(g, b, "{:?} drifted off the hue", (r, g, b));
        }
        // And it is a real climb, not a band of near-identical reds.
        assert!(
            lum(stops[5].1) > 2.5 * lum(stops[0].1),
            "{stops:?} barely travels"
        );
    }

    /// Positions are [`FLAME`]'s, so the banding a cover ramp shows is the
    /// same banding the built-in one does.
    #[test]
    fn a_cover_ramp_keeps_the_flame_spacing() {
        let stops = cover_stops([BLUE, YELLOW]);
        let pos = |s: &Stops| s.iter().map(|&(p, _)| p).collect::<Vec<_>>();
        assert_eq!(pos(&stops), pos(&FLAME));
    }

    /// No cover ramp installed, so the built-in one applies — and this is what
    /// every other test in the crate is drawing against.
    #[test]
    fn the_built_in_ramp_stands_when_no_cover_is_installed() {
        assert_eq!(viz_pair(), None);
        assert_eq!(ramp_color(&stops(), 0.0), FLAME[0].1);
        assert_eq!(ramp_color(&stops(), 1.0), FLAME[5].1);
    }

    #[test]
    fn packing_round_trips_a_colour_and_its_absence() {
        assert_eq!(unpack(pack(Some([1, 2, 3]))), Some([1, 2, 3]));
        assert_eq!(unpack(pack(Some([0, 0, 0]))), Some([0, 0, 0]));
        assert_eq!(unpack(pack(Some([255, 255, 255]))), Some([255, 255, 255]));
        assert_eq!(unpack(pack(None)), None);
    }
}
