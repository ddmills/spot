//! Plays an internet radio stream.
//!
//! Spotify audio reaches the speakers through librespot's player; a radio
//! station cannot, because it is nothing to do with Spotify — it is an HTTP
//! response that never ends. So spot decodes it itself:
//!
//! ```text
//! reqwest ─▶ stream-download   a bounded ring, exposed as Read + Seek
//!         └▶ icy-metadata      lifts the StreamTitle out of the byte stream
//!            └▶ rodio/symphonia  decodes MP3 or AAC to PCM
//!               └▶ TapSource   tees the PCM into the visualizer's AudioTap
//!                  └▶ rodio Sink ─▶ cpal
//! ```
//!
//! The two engines are mutually exclusive by construction — `client` stops one
//! before starting the other — so they never contend for the output device.
//!
//! Everything from the decoder down runs on a dedicated OS thread: rodio's
//! `OutputStream` wraps a cpal stream that is not `Send` on Windows, and the
//! decoder's `Read` calls block on the network. The tokio side does the
//! connecting and hands the finished reader across.

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use icy_metadata::{IcyHeaders, IcyMetadataReader, RequestIcyMetadata};
use parking_lot::Mutex;
use rodio::source::Source;
use rodio::{OutputStream, OutputStreamBuilder, Sink};
use stream_download::http::HttpStream;
use stream_download::http::reqwest::Client as IcyClient;
use stream_download::storage::bounded::BoundedStorageProvider;
use stream_download::storage::memory::MemoryStorageProvider;
use stream_download::{Settings, StreamDownload};

use crate::audio_tap::AudioTap;

/// Bytes of stream held at once. A broadcast has no end, so the buffer has to
/// be a ring rather than a file that grows all evening — but it must still be
/// roomy enough that symphonia's probe, which seeks backwards over the first
/// frames to identify the codec, never reads off the back of it.
const BUFFER_BYTES: usize = 512 * 1024;

/// Seconds of audio to collect before handing the reader to the decoder.
/// Under this the decoder starves on the first hiccup; much over it and the
/// station takes visibly long to start.
const PREFETCH_SECONDS: u64 = 5;

/// Assumed bitrate, in kilobits, when the server does not report one. Only
/// used to size the prefetch.
const ASSUMED_KBPS: u64 = 128;

/// Samples buffered before they are handed to the visualizer, as interleaved
/// stereo. 256 frames is ~6 ms at 44.1 kHz: short enough that the bars track
/// the music, long enough that the tap's lock is taken rarely.
const TAP_FLUSH: usize = 512;

/// The concrete reader the decoder gets. Spelled out because rodio's decoder
/// wants a `Read + Seek` type, not a trait object.
type RadioReader = IcyMetadataReader<StreamDownload<BoundedStorageProvider<MemoryStorageProvider>>>;

enum AudioCmd {
    /// Play a connected stream. Carries the generation it was opened for, so a
    /// station the user has already moved on from is dropped rather than
    /// played over the one they chose instead.
    Play {
        reader: Box<RadioReader>,
        generation: u64,
        volume: f32,
    },
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
}

/// Handle on the radio audio thread.
pub struct RadioPlayer {
    tx: Sender<AudioCmd>,
    /// The station's current track, as the server last announced it. Written
    /// from the decoder thread by the icy callback and read by the UI every
    /// frame, so it is its own lock and not a field of `AppState`.
    title: Arc<Mutex<Option<String>>>,
    tap: Arc<AudioTap>,
    /// Bumped on every play and stop. A connection that completes after its
    /// generation has passed is discarded.
    generation: Arc<AtomicU64>,
    /// Whether a stream has been handed to the audio thread and not stopped.
    ///
    /// The engine's own liveness, deliberately not read off
    /// `AppState.radio`. That field is a UI fact — the event layer clears it on
    /// the click that starts a track, so the deck stops drawing a station that
    /// is going away — and for one turn of the command channel it says "no
    /// station" while this thread is still streaming one. Asking the UI whether
    /// to stop the audio is what let both engines play at once.
    live: Arc<AtomicBool>,
}

impl RadioPlayer {
    /// Spawns the audio thread. The output device is not opened until the
    /// first station plays — a session that never touches radio should not
    /// hold a device open for it.
    pub fn new(tap: Arc<AudioTap>) -> Self {
        let (tx, rx) = channel();
        let thread_tap = Arc::clone(&tap);
        let generation = Arc::new(AtomicU64::new(0));
        let thread_generation = Arc::clone(&generation);
        std::thread::Builder::new()
            .name("spot-radio".to_string())
            .spawn(move || audio_thread(rx, thread_tap, thread_generation))
            .expect("failed to spawn the radio audio thread");
        Self {
            tx,
            title: Arc::new(Mutex::new(None)),
            tap,
            generation,
            live: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether this thread is streaming a station.
    ///
    /// True from the moment a connected stream is handed to the audio thread
    /// until [`Self::stop`]. See [`Self::live`] for why the caller must ask
    /// here rather than looking at `AppState.radio`.
    pub fn is_live(&self) -> bool {
        self.live.load(Ordering::SeqCst)
    }

    /// The shared now-playing slot. Cloned into `AppState` so the deck can read
    /// it without going through the client.
    pub fn title(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.title)
    }

    /// Connect to `url` and start playing it.
    ///
    /// Returns once the stream is connected and prefetched, which is also when
    /// the first audio is heard — so the caller can report a failure to the
    /// user instead of leaving them looking at a station that is silently not
    /// playing.
    pub async fn play(&self, url: &str, volume_percent: u8) -> Result<()> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.title.lock() = None;
        self.tap.clear();

        let reader = self.open(url).await?;
        // Opening took as long as it took; the user may have chosen something
        // else in the meantime.
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        // Marked live with the hand-off, not with the request: until the reader
        // reaches the thread there is nothing playing to stop.
        self.live.store(true, Ordering::SeqCst);
        self.send(AudioCmd::Play {
            reader: Box::new(reader),
            generation,
            volume: amplitude(volume_percent),
        });
        Ok(())
    }

    async fn open(&self, url: &str) -> Result<RadioReader> {
        let url = url
            .parse()
            .with_context(|| format!("the station's address is not a URL: {url}"))?;
        // `request_icy_metadata` sets `Icy-MetaData: 1`, which is what makes
        // the server interleave track titles into the audio. It has to be on
        // the GET: many Icecast servers answer HEAD with an HTML page and no
        // icy headers at all, so probing separately would learn nothing.
        let client = IcyClient::builder()
            .request_icy_metadata()
            .build()
            .context("could not build the radio HTTP client")?;
        let stream = HttpStream::new(client, url)
            .await
            .map_err(|e| anyhow!("could not reach the station: {e}"))?;

        let headers = IcyHeaders::parse_from_headers(stream.headers());
        let kbps = headers.bitrate().map_or(ASSUMED_KBPS, u64::from);
        let prefetch = kbps / 8 * 1024 * PREFETCH_SECONDS;

        let storage = BoundedStorageProvider::new(
            MemoryStorageProvider,
            NonZeroUsize::new(BUFFER_BYTES).expect("buffer size is not zero"),
        );
        let reader = StreamDownload::from_stream(
            stream,
            storage,
            Settings::default().prefetch_bytes(prefetch),
        )
        .await
        .map_err(|e| anyhow!("could not buffer the station: {e}"))?;

        let title = Arc::clone(&self.title);
        Ok(IcyMetadataReader::new(
            reader,
            headers.metadata_interval(),
            move |metadata| {
                let announced = metadata
                    .ok()
                    .and_then(|m| m.stream_title().map(str::to_string))
                    .filter(|t| !t.trim().is_empty());
                *title.lock() = announced;
            },
        ))
    }

    pub fn pause(&self) {
        self.send(AudioCmd::Pause);
    }

    pub fn resume(&self) {
        self.send(AudioCmd::Resume);
    }

    /// Stop playing and close the output device.
    ///
    /// Also bumps the generation, so a station still connecting when this lands
    /// never reaches the sink.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.live.store(false, Ordering::SeqCst);
        *self.title.lock() = None;
        self.send(AudioCmd::Stop);
    }

    pub fn set_volume(&self, percent: u8) {
        self.send(AudioCmd::SetVolume(amplitude(percent)));
    }

    /// A dead channel means the audio thread panicked. There is nothing useful
    /// to do about it from here and no reason to take the app down with it —
    /// radio simply stops working, and the log says why.
    fn send(&self, cmd: AudioCmd) {
        if self.tx.send(cmd).is_err() {
            log::error!("the radio audio thread is gone; the command was dropped");
        }
    }
}

/// Percent to linear amplitude, on a cubic taper.
///
/// A slider that is linear in amplitude spends its top half on changes you can
/// barely hear and its bottom half going from quiet to silent. Cubing it is the
/// usual approximation to a logarithmic fader, and puts half volume at roughly
/// -18 dB, which is close to what the Spotify side of the app does.
fn amplitude(percent: u8) -> f32 {
    let p = f32::from(percent.min(100)) / 100.0;
    p * p * p
}

/// Owns the output device and the sink for as long as a station is playing.
fn audio_thread(rx: Receiver<AudioCmd>, tap: Arc<AudioTap>, generation: Arc<AtomicU64>) {
    // Opened on the first Play and dropped on Stop, so the device is held only
    // while it is in use.
    let mut out: Option<(OutputStream, Sink)> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            AudioCmd::Play {
                reader,
                generation: sent_at,
                volume,
            } => {
                // Checked again here, not only before the send: a Stop issued
                // in the window between the two would otherwise be undone by
                // the Play queued behind it, and the station the user just
                // stopped would start.
                if generation.load(Ordering::SeqCst) != sent_at {
                    continue;
                }
                if let Err(e) = start(&mut out, *reader, volume, &tap) {
                    log::error!("radio playback failed: {e:#}");
                    out = None;
                    tap.clear();
                }
            }
            AudioCmd::Pause => {
                if let Some((_, sink)) = &out {
                    sink.pause();
                    // The tap's freshness check is what tells the visualizer to
                    // rest; without this it would keep drawing the last bars
                    // for as long as the ring held samples.
                    tap.clear();
                }
            }
            AudioCmd::Resume => {
                if let Some((_, sink)) = &out {
                    sink.play();
                }
            }
            AudioCmd::Stop => {
                out = None;
                tap.clear();
            }
            AudioCmd::SetVolume(v) => {
                if let Some((_, sink)) = &out {
                    sink.set_volume(v);
                }
            }
        }
    }
}

fn start(
    out: &mut Option<(OutputStream, Sink)>,
    reader: RadioReader,
    volume: f32,
    tap: &Arc<AudioTap>,
) -> Result<()> {
    // Building the decoder reads the first frames to identify the codec, which
    // is why the reader was prefetched before it got here.
    let decoder = rodio::Decoder::builder()
        .with_data(reader)
        // A broadcast has no beginning to seek back to. Saying so stops
        // symphonia looking for a seek index that cannot exist.
        .with_seekable(false)
        .build()
        .context("could not decode the station's audio")?;

    if out.is_none() {
        let mut stream = OutputStreamBuilder::open_default_stream()
            .context("could not open an audio output device")?;
        // rodio's `Drop` otherwise `eprintln!`s a line about the stream
        // closing — straight into the alternate screen, over whatever the UI
        // had drawn there, every time a station is stopped.
        stream.log_on_drop(false);
        let sink = Sink::connect_new(stream.mixer());
        *out = Some((stream, sink));
    }
    let (_, sink) = out.as_ref().expect("the output was just opened");

    // Whatever was playing goes now, not when the new source runs out.
    sink.clear();
    sink.set_volume(volume);
    sink.append(TapSource::new(decoder, Arc::clone(tap)));
    sink.play();
    Ok(())
}

/// Copies PCM into the visualizer's tap on its way to the sink.
///
/// The same job `audio_tap::TapSink` does for librespot, one layer further out:
/// there the tee sits in librespot's `Sink`, here it sits in rodio's `Source`.
/// Both end at the same [`AudioTap`], so `viz` neither knows nor cares which
/// engine is playing.
///
/// It sits *above* rodio's `Sink`, which is where volume is applied, so what
/// the visualizer sees is volume-agnostic — the same property `TapSink` gets by
/// dividing the mixer's attenuation back out.
struct TapSource<S> {
    inner: S,
    tap: Arc<AudioTap>,
    /// Interleaved stereo, flushed at [`TAP_FLUSH`].
    buf: Vec<f64>,
    /// Cached from `inner`: a mono station has to have each sample written
    /// twice, because the tap reads its input in stereo pairs.
    mono: bool,
}

impl<S: Source> TapSource<S> {
    fn new(inner: S, tap: Arc<AudioTap>) -> Self {
        let mono = inner.channels() == 1;
        Self {
            inner,
            tap,
            buf: Vec::with_capacity(TAP_FLUSH),
            mono,
        }
    }

    fn flush(&mut self) {
        // Gain 1.0: nothing has attenuated these samples yet.
        self.tap.push(&self.buf, 1.0);
        self.buf.clear();
    }
}

impl<S: Source> Iterator for TapSource<S> {
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.inner.next()?;
        self.buf.push(f64::from(sample));
        if self.mono {
            self.buf.push(f64::from(sample));
        }
        if self.buf.len() >= TAP_FLUSH {
            self.flush();
        }
        Some(sample)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<S: Source> Source for TapSource<S> {
    fn current_span_len(&self) -> Option<usize> {
        self.inner.current_span_len()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Liveness is the engine's own, not a reading of `AppState.radio`.
    ///
    /// The two say different things for the turn of the command channel between
    /// a track being clicked — where the event layer clears `AppState.radio` so
    /// the deck stops drawing the station at once — and the client getting to
    /// the play. `yield_to_spotify` asked the UI over that window and so never
    /// stopped the stream, and both engines played at once.
    #[test]
    fn liveness_follows_the_stream_not_the_ui() {
        let player = RadioPlayer::new(Arc::new(AudioTap::new()));
        assert!(!player.is_live(), "nothing has been handed to the thread");

        // Stand in for a connected stream reaching the audio thread; `play`
        // itself needs a network and a device.
        player.live.store(true, Ordering::SeqCst);
        assert!(player.is_live());

        player.stop();
        assert!(!player.is_live(), "stop is what ends it");
    }

    #[test]
    fn volume_is_cubic_and_clamped() {
        assert_eq!(amplitude(0), 0.0);
        assert_eq!(amplitude(100), 1.0);
        assert_eq!(amplitude(200), 1.0);
        assert!((amplitude(50) - 0.125).abs() < 1e-6);
    }

    /// A source rodio can drive, so the tap wrapper can be exercised without
    /// an audio device.
    struct Tone {
        samples: std::vec::IntoIter<f32>,
        channels: u16,
    }

    impl Iterator for Tone {
        type Item = f32;
        fn next(&mut self) -> Option<f32> {
            self.samples.next()
        }
    }

    impl Source for Tone {
        fn current_span_len(&self) -> Option<usize> {
            None
        }
        fn channels(&self) -> rodio::ChannelCount {
            self.channels
        }
        fn sample_rate(&self) -> rodio::SampleRate {
            44_100
        }
        fn total_duration(&self) -> Option<Duration> {
            None
        }
    }

    fn tone(samples: Vec<f32>, channels: u16) -> Tone {
        Tone {
            samples: samples.into_iter(),
            channels,
        }
    }

    #[test]
    fn tap_source_passes_samples_through_unchanged() {
        let tap = Arc::new(AudioTap::new());
        let src = TapSource::new(tone(vec![0.25, -0.25, 1.0, -1.0], 2), Arc::clone(&tap));
        assert_eq!(src.collect::<Vec<_>>(), vec![0.25, -0.25, 1.0, -1.0]);
    }

    #[test]
    fn tap_source_fills_the_tap() {
        let tap = Arc::new(AudioTap::new());
        // Enough samples to cross the flush threshold; the tap only sees
        // whole flushes.
        let samples: Vec<f32> = (0..TAP_FLUSH).map(|_| 0.5).collect();
        let mut src = TapSource::new(tone(samples, 2), Arc::clone(&tap));
        for _ in 0..TAP_FLUSH {
            src.next();
        }
        let mut out = Vec::new();
        tap.latest(&mut out, TAP_FLUSH);
        // Interleaved stereo is downmixed to mono by the tap: 0.5 and 0.5
        // average to 0.5, half as many samples out as in.
        assert_eq!(out.len(), TAP_FLUSH / 2);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    /// Connects to a real station, decodes it, and plays it through the
    /// speakers for a few seconds.
    ///
    /// Ignored by default: it needs a network, an audio device, and someone
    /// listening. It is the only check that covers the whole chain — the ICY
    /// request, the bounded buffer, symphonia's probe, and the tap — so run it
    /// with `cargo test radio::player::tests::live_ -- --ignored --nocapture`
    /// after touching any of them. It prints whether PCM reached the tap,
    /// which is what the visualizer reads.
    #[tokio::test]
    #[ignore]
    async fn live_station_plays_and_feeds_the_tap() {
        // SomaFM Groove Salad: plain MP3 over ICY, and stable enough to hang a
        // test on.
        const URL: &str = "https://ice1.somafm.com/groovesalad-128-mp3";

        let tap = Arc::new(AudioTap::new());
        let player = RadioPlayer::new(Arc::clone(&tap));
        player.play(URL, 40).await.expect("the station should play");

        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            tap.is_fresh(Duration::from_secs(1)),
            "no PCM reached the tap, so the visualizer would sit dead"
        );
        let mut out = Vec::new();
        tap.latest(&mut out, 2048);
        assert!(
            out.iter().any(|s| s.abs() > 1e-4),
            "the tap holds only silence"
        );
        println!("stream title: {:?}", player.title().lock());

        player.stop();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !tap.is_fresh(Duration::from_millis(200)),
            "stopping must make the tap go stale, or the bars keep dancing"
        );
    }

    /// A mono station still has to read back as stereo pairs, or the tap would
    /// average two unrelated samples together and halve the pitch of the
    /// spectrum.
    #[test]
    fn tap_source_duplicates_mono_samples() {
        let tap = Arc::new(AudioTap::new());
        let samples: Vec<f32> = (0..TAP_FLUSH / 2).map(|i| i as f32).collect();
        let mut src = TapSource::new(tone(samples, 1), Arc::clone(&tap));
        for _ in 0..TAP_FLUSH / 2 {
            src.next();
        }
        let mut out = Vec::new();
        tap.latest(&mut out, TAP_FLUSH);
        assert_eq!(out.len(), TAP_FLUSH / 2);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 1.0);
    }
}
