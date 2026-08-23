//! Album cover art: fetch, decode, and reduce to a small square of RGB the
//! player can paint with half-block cells.
//!
//! The cover URL rides along with every playback poll (see
//! [`crate::api::snapshot_from_context`]), so art costs no extra Web API call
//! — only a GET against Spotify's CDN, which is outside the shared API quota.
//!
//! Everything here treats the image as untrusted remote data: the host is
//! checked against an allowlist, the body is capped before it is buffered, and
//! the decoder is given hard dimension limits so a small file cannot expand
//! into a huge allocation.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Side of the decoded cover, in pixels. Comfortably above the widest art
/// block the player draws (24 px), so the draw-time resample is always a real
/// area average and never an upscale.
pub const COVER_PX: usize = 64;

/// Cap on the response body. A 300 px Spotify cover is ~30 KB; a megabyte is
/// far past anything legitimate and well under anything that would hurt.
const MAX_COVER_BYTES: usize = 1 << 20;
/// Hard limit handed to the decoder. Enforced there rather than only after
/// the fact, so a decode bomb is refused before it allocates.
const MAX_SOURCE_PX: usize = 2048;
/// Decoded covers kept by URL. 16 * 64*64*3 = 192 KB.
const CACHE_MAX: usize = 16;
/// Covers the artist page's own store keeps: a full page of album cards
/// (`api::PAGE_LIMIT`), the artist photo, and room to browse back to the
/// artist you came from without re-fetching a sleeve. 128 * 12 KB = 1.5 MB.
pub const PAGE_ART_MAX: usize = 128;

/// A decoded cover at a fixed square resolution. The player box-filters it
/// down to whatever the pane can spare, so the art tracks terminal resizes
/// without a re-fetch.
pub struct Cover {
    pub url: String,
    /// Row-major RGB, `size * size` entries.
    pub px: Vec<[u8; 3]>,
    pub size: usize,
    /// The cover's dominant saturated colour, for the UI accent. `None` for a
    /// greyscale or near-monochrome sleeve, where any pick would be arbitrary.
    pub accent: Option<[u8; 3]>,
    /// Two well-separated hues off the sleeve, darker first, for the
    /// visualizer's gradient. `None` when the cover only has one hue worth
    /// having and the built-in ramp should stand.
    pub ramp: Option<[[u8; 3]; 2]>,
}

impl Cover {
    /// Box-filter down to a `cols` x `2 * rows` pixel grid — the shape a block
    /// of half-block (`▀`) cells holds, two stacked pixels per cell.
    pub fn block(&self, cols: usize, rows: usize) -> Vec<[u8; 3]> {
        resample(&self.px, self.size, cols, rows * 2)
    }
}

/// Smallest image at least twice [`COVER_PX`] on its short side — Spotify
/// serves 640/300/64, so this picks the 300: four times less to download and
/// decode than the 640, and still a genuine downscale into the 64 px grid.
/// Falls back to the largest available when nothing is big enough.
pub fn pick_url(images: &[rspotify::model::Image]) -> Option<String> {
    let sized: Vec<(&str, u32)> = images
        .iter()
        .map(|i| {
            (
                i.url.as_str(),
                i.width.unwrap_or(0).min(i.height.unwrap_or(0)),
            )
        })
        .collect();
    pick_sized(&sized)
}

/// [`pick_url`]'s rule, over `(url, short side in pixels)` pairs.
///
/// Spelled separately because the same choice has to be made about two
/// unrelated types: rspotify's `Image`, off the Web API, and librespot's
/// `CoverImage`, which rides along with the `TrackChanged` its player emits the
/// moment a record starts. The two must land on the same file or a track change
/// fetches one sleeve and the poll behind it fetches another.
pub fn pick_sized(images: &[(&str, u32)]) -> Option<String> {
    images
        .iter()
        .filter(|(_, short)| *short as usize >= 2 * COVER_PX)
        .min_by_key(|(_, short)| *short)
        .or_else(|| images.iter().max_by_key(|(_, short)| *short))
        .map(|(url, _)| (*url).to_string())
}

/// Whether `url` points at Spotify's image CDN over TLS.
///
/// The URL is remote data, and this is the only thing standing between an
/// unexpected value in an API response and the client fetching an arbitrary
/// host. Deliberately a string check rather than a URL parser: the shapes it
/// has to reject are few, and a dependency here would be the larger risk.
pub fn is_spotify_cdn(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // `https://i.scdn.co@evil.com/x` has authority `i.scdn.co@evil.com`, whose
    // real host is `evil.com`. Rejecting userinfo outright is simpler than
    // parsing it, and no legitimate CDN URL has any.
    if authority.contains('@') {
        return false;
    }
    let host = authority.split(':').next().unwrap_or("");
    let suffixed = |suffix: &str| {
        host.len() > suffix.len() && host[host.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    };
    host.eq_ignore_ascii_case("i.scdn.co") || suffixed(".scdn.co") || suffixed(".spotifycdn.com")
}

/// Fetch and decode the cover at `url`. The decode runs on the blocking pool:
/// it is CPU-bound and its input is remote bytes.
pub async fn load(http: &reqwest::Client, url: &str) -> Result<Cover> {
    let mut resp = http.get(url).send().await?.error_for_status()?;
    if resp
        .content_length()
        .is_some_and(|n| n > MAX_COVER_BYTES as u64)
    {
        bail!("cover art too large");
    }
    // Content-Length is remote and optional, so cap the actual read as well.
    let mut bytes = Vec::with_capacity(64 * 1024);
    while let Some(chunk) = resp.chunk().await? {
        if bytes.len() + chunk.len() > MAX_COVER_BYTES {
            bail!("cover art too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    let url = url.to_string();
    tokio::task::spawn_blocking(move || decode(&bytes, url))
        .await
        .context("cover decode task panicked")?
}

/// Decode JPEG bytes into a [`COVER_PX`]-square cover. Pure and blocking, so
/// it can be unit-tested without a network or a runtime.
fn decode(bytes: &[u8], url: String) -> Result<Cover> {
    let options = DecoderOptions::default()
        .set_strict_mode(false)
        .set_max_width(MAX_SOURCE_PX)
        .set_max_height(MAX_SOURCE_PX)
        .jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|e| anyhow!("jpeg headers: {e:?}"))?;
    let info = decoder.info().context("jpeg has no header info")?;
    let (w, h) = (info.width as usize, info.height as usize);
    // The decoder's own limits should already have caught this; check anyway,
    // because the next line allocates on these numbers.
    if w == 0 || h == 0 || w > MAX_SOURCE_PX || h > MAX_SOURCE_PX {
        bail!("cover art dimensions out of range: {w}x{h}");
    }
    let comps = decoder
        .output_colorspace()
        .context("jpeg has no output colorspace")?
        .num_components();
    let raw = decoder.decode().map_err(|e| anyhow!("jpeg: {e:?}"))?;
    if raw.len() < w * h * comps {
        bail!(
            "jpeg decoded short: {} bytes for {w}x{h}x{comps}",
            raw.len()
        );
    }

    let px = square_downsample(&raw, comps, w, h, COVER_PX);
    Ok(Cover {
        accent: dominant(&px),
        ramp: palette(&px),
        url,
        px,
        size: COVER_PX,
    })
}

/// Centre-crop `w` x `h` interleaved samples to a square, then box-filter to
/// `out` x `out` RGB. Spotify's covers are already square; the crop is there
/// so an odd one is framed rather than stretched.
fn square_downsample(raw: &[u8], comps: usize, w: usize, h: usize, out: usize) -> Vec<[u8; 3]> {
    let side = w.min(h);
    let (ox, oy) = ((w - side) / 2, (h - side) / 2);
    let at = |x: usize, y: usize| {
        let i = ((y + oy) * w + (x + ox)) * comps;
        match comps {
            // Greyscale JPEGs decode to one component per pixel.
            1 => [raw[i], raw[i], raw[i]],
            _ => [raw[i], raw[i + 1], raw[i + 2]],
        }
    };
    box_filter(side, side, out, out, at)
}

/// Box-filter a square RGB buffer down to `cols` x `rows`.
fn resample(px: &[[u8; 3]], size: usize, cols: usize, rows: usize) -> Vec<[u8; 3]> {
    box_filter(size, size, cols, rows, |x, y| px[y * size + x])
}

/// Area-average `src_w` x `src_h` down to `cols` x `rows`. Each output pixel
/// is the mean of the source rectangle it covers, which is the right filter
/// for the 5x-ish reductions this module does — nearest aliases badly on album
/// art, and a windowed filter would ring on the hard edges sleeves are full of.
fn box_filter(
    src_w: usize,
    src_h: usize,
    cols: usize,
    rows: usize,
    at: impl Fn(usize, usize) -> [u8; 3],
) -> Vec<[u8; 3]> {
    if cols == 0 || rows == 0 || src_w == 0 || src_h == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(cols * rows);
    for ty in 0..rows {
        // `+ 1` on the far edge so a source smaller than the target still
        // covers at least one pixel per cell instead of an empty range.
        let y0 = ty * src_h / rows;
        let y1 = (((ty + 1) * src_h).div_ceil(rows)).max(y0 + 1).min(src_h);
        for tx in 0..cols {
            let x0 = tx * src_w / cols;
            let x1 = (((tx + 1) * src_w).div_ceil(cols)).max(x0 + 1).min(src_w);
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = at(x, y);
                    r += p[0] as u32;
                    g += p[1] as u32;
                    b += p[2] as u32;
                    n += 1;
                }
            }
            let n = n.max(1);
            out.push([(r / n) as u8, (g / n) as u8, (b / n) as u8]);
        }
    }
    out
}

/// Hue buckets the dominant-colour histogram uses, 15 degrees each.
const HUE_BUCKETS: usize = 24;
/// A pixel below this saturation is grey as far as the accent is concerned,
/// and one outside the value band is too close to black or white to carry a
/// hue at all. Both would drag the average toward mud.
const MIN_SAT: f32 = 0.25;
const MIN_VAL: f32 = 0.15;
const MAX_VAL: f32 = 0.95;
/// The winning hue must hold at least this share of the image, or the sleeve
/// has no colour worth adopting and the static accent stays.
const MIN_SHARE: f32 = 0.02;
/// The accent is UI foreground, so it is pushed to a floor of saturation and
/// brightness. A dark or washed-out sleeve still yields something readable.
pub const ACCENT_SAT: f32 = 0.55;
const ACCENT_VAL: f32 = 0.72;
/// How far apart on the hue circle [`palette`]'s two picks must sit, in
/// buckets. Two is 30 degrees — enough that the gradient between them is a
/// visible sweep rather than two shades of the same colour. Closer than that
/// and the second pick is not worth having, because the ramp built from one
/// hue alone (see [`palette`]) is the same picture with less to go wrong.
const MIN_HUE_GAP: usize = 2;

/// Per-hue-bucket pixel counts and RGB sums over the colourful pixels of `px`.
///
/// Colour is bucketed by *hue* rather than averaged outright: averaging a
/// whole sleeve converges on grey-brown almost regardless of what is on it.
/// Grey, near-black and near-white pixels are dropped rather than counted,
/// since they carry no hue to bucket.
fn hue_histogram(px: &[[u8; 3]]) -> ([u32; HUE_BUCKETS], [[u32; 3]; HUE_BUCKETS]) {
    let mut counts = [0u32; HUE_BUCKETS];
    let mut sums = [[0u32; 3]; HUE_BUCKETS];
    for p in px {
        let (h, s, v) = to_hsv(*p);
        if s < MIN_SAT || !(MIN_VAL..=MAX_VAL).contains(&v) {
            continue;
        }
        let bucket = ((h / 360.0 * HUE_BUCKETS as f32) as usize).min(HUE_BUCKETS - 1);
        counts[bucket] += 1;
        for c in 0..3 {
            sums[bucket][c] += p[c] as u32;
        }
    }
    (counts, sums)
}

/// A bucket's mean colour.
fn bucket_mean(sums: &[[u32; 3]; HUE_BUCKETS], n: u32, bucket: usize) -> [u8; 3] {
    [
        (sums[bucket][0] / n) as u8,
        (sums[bucket][1] / n) as u8,
        (sums[bucket][2] / n) as u8,
    ]
}

/// Whether a bucket holds enough of the image to be worth adopting.
fn holds_share(n: u32, total: usize) -> bool {
    n > 0 && (n as f32) >= total as f32 * MIN_SHARE
}

/// The cover's dominant saturated hue, lifted to a legible foreground colour.
pub fn dominant(px: &[[u8; 3]]) -> Option<[u8; 3]> {
    let (counts, sums) = hue_histogram(px);
    let (best, &n) = counts.iter().enumerate().max_by_key(|&(_, &n)| n)?;
    if !holds_share(n, px.len()) {
        return None;
    }
    let (h, s, v) = to_hsv(bucket_mean(&sums, n, best));
    Some(from_hsv(h, s.max(ACCENT_SAT), v.max(ACCENT_VAL)))
}

/// The hues the visualizer's gradient is built from: the dominant one (see
/// [`dominant`]) first, then the strongest hue at least [`MIN_HUE_GAP`]
/// buckets away from it. Both are raw bucket means rather than lifted colours
/// — only the hues are wanted here, and the ramp assigns its own saturation
/// and brightness (see [`crate::ui::theme::viz_color`]).
///
/// A sleeve with only one hue in it — an all-red cover, a duotone — gets that
/// hue twice, and the ramp then travels in brightness alone: a red record
/// should light a red meter, not fall back to a green one. `None` only for a
/// greyscale sleeve, which has no hue to travel in and keeps the built-in ramp.
pub fn palette(px: &[[u8; 3]]) -> Option<[[u8; 3]; 2]> {
    let (counts, sums) = hue_histogram(px);
    let (best, &n) = counts.iter().enumerate().max_by_key(|&(_, &n)| n)?;
    if !holds_share(n, px.len()) {
        return None;
    }
    let first = bucket_mean(&sums, n, best);
    // Buckets are a circle, so the distance between two of them is the shorter
    // way round: bucket 23 is one step from bucket 0, not twenty-three.
    let far = |b: usize| {
        let d = b.abs_diff(best);
        d.min(HUE_BUCKETS - d) >= MIN_HUE_GAP
    };
    let second = counts
        .iter()
        .enumerate()
        .filter(|&(b, _)| far(b))
        .max_by_key(|&(_, &n)| n)
        .filter(|&(_, &m)| holds_share(m, px.len()))
        .map_or(first, |(other, &m)| bucket_mean(&sums, m, other));
    Some([first, second])
}

/// Hue in degrees, saturation and value in 0..=1.
pub fn to_hsv([r, g, b]: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let c = max - min;
    let h = if c == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / c) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / c + 2.0)
    } else {
        60.0 * ((r - g) / c + 4.0)
    };
    (
        if h < 0.0 { h + 360.0 } else { h },
        if max == 0.0 { 0.0 } else { c / max },
        max,
    )
}

pub fn from_hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let c = v * s;
    let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let ch = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    [ch(r), ch(g), ch(b)]
}

/// Bounded FIFO of decoded covers by URL, so flipping between two tracks does
/// not re-fetch. FIFO rather than LRU: at this size the hit-rate difference is
/// unmeasurable and it saves a dependency.
pub struct CoverCache {
    map: HashMap<String, Arc<Cover>>,
    order: VecDeque<String>,
    cap: usize,
}

impl Default for CoverCache {
    fn default() -> Self {
        Self::with_capacity(CACHE_MAX)
    }
}

impl CoverCache {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    pub fn get(&self, url: &str) -> Option<Arc<Cover>> {
        self.map.get(url).cloned()
    }

    /// Whether `url` is already decoded, without cloning the cover — what a
    /// fetcher asks before spending a request on it.
    pub fn contains(&self, url: &str) -> bool {
        self.map.contains_key(url)
    }

    pub fn insert(&mut self, url: String, cover: Arc<Cover>) {
        if self.map.insert(url.clone(), cover).is_none() {
            self.order.push_back(url);
        }
        while self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rspotify::model::Image;

    use super::*;

    fn image(w: u32, h: u32, url: &str) -> Image {
        Image {
            url: url.into(),
            width: Some(w),
            height: Some(h),
        }
    }

    #[test]
    fn picks_the_three_hundred_pixel_image() {
        let images = [
            image(640, 640, "big"),
            image(300, 300, "mid"),
            image(64, 64, "small"),
        ];
        assert_eq!(pick_url(&images).as_deref(), Some("mid"));
    }

    /// Nothing meets the size floor, so the largest available is better than
    /// giving up and showing a placeholder.
    #[test]
    fn falls_back_to_the_largest_when_all_are_small() {
        let images = [image(64, 64, "small"), image(32, 32, "tiny")];
        assert_eq!(pick_url(&images).as_deref(), Some("small"));
        assert_eq!(pick_url(&[]), None);
    }

    /// Sizes are optional in the API model; an entry with none must not be
    /// treated as enormous or crash the comparison.
    #[test]
    fn tolerates_images_without_dimensions() {
        let images = [
            Image {
                url: "unknown".into(),
                width: None,
                height: None,
            },
            image(300, 300, "mid"),
        ];
        assert_eq!(pick_url(&images).as_deref(), Some("mid"));
    }

    #[test]
    fn rejects_non_spotify_hosts() {
        assert!(is_spotify_cdn("https://i.scdn.co/image/ab67616d00001e02"));
        assert!(is_spotify_cdn("https://mosaic.scdn.co/300/abc"));
        assert!(is_spotify_cdn(
            "https://image-cdn-ak.spotifycdn.com/image/x"
        ));
        // Plaintext, a lookalike suffix, a bare suffix match, and userinfo
        // hiding the real host behind an `@`.
        assert!(!is_spotify_cdn("http://i.scdn.co/image/x"));
        assert!(!is_spotify_cdn("https://evil.com/image/x"));
        assert!(!is_spotify_cdn("https://i.scdn.co.evil.com/image/x"));
        assert!(!is_spotify_cdn("https://notscdn.co/image/x"));
        assert!(!is_spotify_cdn("https://i.scdn.co@evil.com/image/x"));
        assert!(!is_spotify_cdn("i.scdn.co/image/x"));
    }

    #[test]
    fn box_filter_of_a_solid_image_is_that_colour() {
        let px = vec![[10u8, 20, 30]; 64 * 64];
        assert!(resample(&px, 64, 8, 8).iter().all(|&p| p == [10, 20, 30]));
    }

    #[test]
    fn box_filter_averages_a_split_image() {
        // Black top half, white bottom half, reduced to two rows.
        let px: Vec<[u8; 3]> = (0..64)
            .flat_map(|y| (0..64).map(move |_| if y < 32 { [0u8; 3] } else { [255u8; 3] }))
            .collect();
        let out = resample(&px, 64, 1, 2);
        assert_eq!(out, vec![[0, 0, 0], [255, 255, 255]]);
        // And one row averages the two halves.
        assert_eq!(resample(&px, 64, 1, 1), vec![[127, 127, 127]]);
    }

    /// Every output cell must be filled even when the target grid is finer
    /// than the source, or the art shows holes on a large pane.
    #[test]
    fn box_filter_fills_every_cell_when_upscaling() {
        let px = vec![[7u8, 8, 9]; 4 * 4];
        let out = resample(&px, 4, 10, 6);
        assert_eq!(out.len(), 60);
        assert!(out.iter().all(|&p| p == [7, 8, 9]));
    }

    #[test]
    fn box_filter_handles_a_zero_sized_target() {
        assert!(resample(&[[1u8, 2, 3]], 1, 0, 4).is_empty());
        assert!(resample(&[[1u8, 2, 3]], 1, 4, 0).is_empty());
    }

    #[test]
    fn center_crop_makes_a_non_square_source_square() {
        // 8 wide, 4 tall: red in the middle 4 columns, blue either side. A
        // centre crop keeps only the red.
        let mut raw = Vec::new();
        for _ in 0..4 {
            for x in 0..8 {
                let p: [u8; 3] = if (2..6).contains(&x) {
                    [255, 0, 0]
                } else {
                    [0, 0, 255]
                };
                raw.extend_from_slice(&p);
            }
        }
        let out = square_downsample(&raw, 3, 8, 4, 2);
        assert_eq!(out, vec![[255, 0, 0]; 4]);
    }

    #[test]
    fn greyscale_sources_decode_as_grey_not_garbage() {
        let raw = vec![128u8; 16 * 16];
        assert_eq!(
            square_downsample(&raw, 1, 16, 16, 2),
            vec![[128, 128, 128]; 4]
        );
    }

    /// A sleeve with no colour in it must not yield a muddy near-black accent;
    /// falling back to the static accent is the better answer.
    #[test]
    fn dominant_colour_ignores_greyscale_covers() {
        let px: Vec<[u8; 3]> = (0..64 * 64).map(|i| [(i % 256) as u8; 3]).collect();
        assert_eq!(dominant(&px), None);
        assert_eq!(dominant(&vec![[0u8, 0, 0]; 256]), None);
        assert_eq!(dominant(&[]), None);
    }

    #[test]
    fn dominant_colour_finds_the_hue_and_lifts_it() {
        // A dark red sleeve: the hue survives, the brightness is raised so the
        // accent is legible as a foreground.
        let px = vec![[70u8, 10, 10]; 256];
        let accent = dominant(&px).expect("a red cover has a dominant hue");
        assert!(
            accent[0] > accent[1] && accent[0] > accent[2],
            "{accent:?} is not red"
        );
        assert!(accent[0] > 150, "accent was not lifted: {accent:?}");
    }

    /// Two hues far enough apart to make a gradient, dominant one first.
    #[test]
    fn palette_takes_the_two_strongest_separated_hues() {
        // Dark blue over pale yellow — opposite sides of the hue circle.
        let mut px = vec![[20u8, 30, 120]; 600];
        px.extend(std::iter::repeat_n([230u8, 220, 90], 400));
        let [first, second] = palette(&px).expect("two distinct hues");
        assert!(first[2] > first[0], "{first:?} is not the blue");
        assert!(second[0] > second[2], "{second:?} is not the yellow");
        // The first pick is the same hue the accent takes.
        let accent = dominant(&px).expect("a dominant hue");
        assert!(accent[2] > accent[0], "{accent:?} is not the blue");
    }

    /// A sleeve with one hue in it gets that hue twice, so an all-red cover
    /// still lights a red meter rather than falling back to the green one.
    #[test]
    fn palette_repeats_the_only_hue_a_sleeve_has() {
        let red = palette(&vec![[180u8, 30, 30]; 1000]).expect("red is a hue");
        assert_eq!(red[0], red[1]);
        assert!(red[0][0] > red[0][1], "{red:?} is not red");
        // Two blues fifteen degrees apart: distinct buckets, far too close to
        // gradient between, so the dominant one stands in for both.
        let mut px = vec![[20u8, 30, 120]; 600];
        px.extend(std::iter::repeat_n([20u8, 60, 120], 400));
        let blue = palette(&px).expect("blue is a hue");
        assert_eq!(blue[0], blue[1]);
    }

    /// A second hue that only a handful of pixels carry is noise, not a colour
    /// the sleeve is made of, so it does not get half the ramp.
    #[test]
    fn palette_ignores_a_hue_too_small_to_count() {
        let mut px = vec![[20u8, 30, 120]; 1000];
        px.extend(std::iter::repeat_n([230u8, 60, 40], 5));
        let out = palette(&px).expect("blue is a hue");
        assert_eq!(out[0], out[1]);
        assert!(out[0][2] > out[0][0], "{out:?} is not blue");
    }

    /// No hue at all: nothing to travel in, so the built-in ramp stands.
    #[test]
    fn palette_declines_a_greyscale_sleeve() {
        assert_eq!(palette(&vec![[90u8; 3]; 1000]), None);
        assert_eq!(palette(&vec![[0u8; 3]; 256]), None);
        assert_eq!(palette(&[]), None);
    }

    /// A splash of colour on a grey field still wins, because grey pixels are
    /// discarded rather than averaged in.
    #[test]
    fn dominant_colour_survives_a_mostly_grey_cover() {
        let mut px = vec![[90u8, 90, 90]; 1000];
        px.extend(std::iter::repeat_n([20u8, 120, 200], 50));
        let accent = dominant(&px).expect("the blue splash should win");
        assert!(accent[2] > accent[0], "{accent:?} is not blue");
    }

    #[test]
    fn hsv_round_trips() {
        for colour in [
            [255u8, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [12, 200, 90],
            [90, 90, 90],
        ] {
            let (h, s, v) = to_hsv(colour);
            let back = from_hsv(h, s, v);
            for c in 0..3 {
                assert!(colour[c].abs_diff(back[c]) <= 1, "{colour:?} -> {back:?}");
            }
        }
    }

    #[test]
    fn rejects_bytes_that_are_not_a_jpeg() {
        assert!(decode(b"not a jpeg at all", "x".into()).is_err());
        assert!(decode(&[], "x".into()).is_err());
    }

    #[test]
    fn cache_evicts_in_insertion_order() {
        let cover = |url: &str| {
            Arc::new(Cover {
                url: url.into(),
                px: Vec::new(),
                size: 0,
                accent: None,
                ramp: None,
            })
        };
        let mut cache = CoverCache::default();
        for i in 0..CACHE_MAX + 4 {
            let url = format!("u{i}");
            cache.insert(url.clone(), cover(&url));
        }
        assert!(cache.get("u0").is_none(), "oldest entry survived eviction");
        assert!(cache.get("u3").is_none());
        assert!(cache.get("u4").is_some());
        assert!(cache.get(&format!("u{}", CACHE_MAX + 3)).is_some());
        assert_eq!(cache.map.len(), CACHE_MAX);
    }

    /// Re-inserting a URL already held must refresh it in place rather than
    /// queue a second eviction entry that would later drop the live one.
    #[test]
    fn cache_reinsert_does_not_double_count() {
        let cover = |url: &str| {
            Arc::new(Cover {
                url: url.into(),
                px: Vec::new(),
                size: 0,
                accent: None,
                ramp: None,
            })
        };
        let mut cache = CoverCache::default();
        for _ in 0..CACHE_MAX + 4 {
            cache.insert("same".into(), cover("same"));
        }
        assert_eq!(cache.order.len(), 1);
        assert!(cache.get("same").is_some());
    }
}
