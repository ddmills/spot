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
//! The output device lives on a dedicated OS thread: rodio's `OutputStream`
//! wraps a cpal stream that is not `Send` on Windows. That thread does nothing
//! else. The tokio side connects the stream and builds the decoder — both of
//! which read the network, and neither of which has an upper bound but the
//! timeouts on the HTTP client — and hands the finished source across.
//!
//! The division is the point. The audio thread is the only thing that can
//! silence the radio, so it must never wait on the network: a read that never
//! returns there is a pause key, a stop key and a quit that never work again.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, OnceLock};
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

/// How long the station has to answer the GET.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connected station may go without sending a byte before it counts
/// as dead.
///
/// This is the timeout that matters. A `Read` on the stream blocks on a
/// condvar that only a byte or the end of the download can release, so a server
/// that accepts the connection and then goes quiet — which happens often
/// enough in a directory of ten thousand stations — used to block whichever
/// thread was reading, for ever. That thread is the decoder's, and a decoder
/// thread that never returns is a radio that cannot be paused, stopped, or
/// changed. Per-read rather than a total timeout: a broadcast never ends, so
/// `Client::timeout` would cut off a healthy station mid-song.
const READ_TIMEOUT: Duration = Duration::from_secs(15);

/// The concrete reader the decoder gets. Spelled out because rodio's decoder
/// wants a `Read + Seek` type, not a trait object.
type RadioReader = IcyMetadataReader<StreamDownload<BoundedStorageProvider<MemoryStorageProvider>>>;

/// What the audio thread is handed: a decoder, already built, with the
/// visualizer's tap around it.
///
/// The decoder is built before the hand-off and not by the audio thread,
/// because building one reads the first frames off the network and so takes as
/// long as the station makes it take. See [`AudioCmd`].
type RadioSource = TapSource<rodio::Decoder<RadioReader>>;

/// How long a shutdown waits for the audio thread to confirm the device is
/// closed. Long enough for a thread that is mid-command, short enough that a
/// wedged one never holds up the quit — the caller exits the process either
/// way.
const SHUTDOWN_ACK_TIMEOUT: Duration = Duration::from_millis(500);

/// A clone of the live thread's command sender, for callers that have no
/// [`RadioPlayer`] to hand — specifically the Windows console-control handler,
/// which runs on its own thread with no tokio runtime under it and cannot
/// reach the one the client owns.
static SHUTDOWN_HOOK: OnceLock<Sender<AudioCmd>> = OnceLock::new();

/// A command for the audio thread.
///
/// Every one of these must be cheap to carry out. The thread that handles them
/// is the only thing that can silence the radio, so anything it blocks on is
/// time in which pause, stop and quit do nothing — which is why the decoder is
/// built before [`AudioCmd::Play`] is sent, and why nothing here reads from the
/// network.
enum AudioCmd {
    /// Play a stream that is connected and decoding. Carries the generation it
    /// was opened for, so a station the user has already moved on from is
    /// dropped rather than played over the one they chose instead.
    Play {
        source: Box<RadioSource>,
        generation: u64,
        volume: f32,
    },
    Pause,
    Resume,
    Stop,
    SetVolume(f32),
    /// Close the device and end the thread. The ack is what makes this
    /// synchronous: the sender knows the output is silent when it arrives.
    Shutdown(Sender<()>),
}

/// Handle on the radio audio thread.
///
/// Cloneable, and every clone is the same thread: the client hands one to each
/// station task it spawns, so connecting a station never holds up the command
/// loop that has to be free to stop it.
#[derive(Clone)]
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
    /// Whether the user has asked for silence, shared with the audio thread.
    ///
    /// A station takes seconds to connect, and the pause key works throughout
    /// them — the deck is already drawn and already says the station is on. The
    /// audio thread has no sink to pause over that window, so the intent is
    /// recorded here instead and read when the stream finally arrives.
    /// Without it, pausing a station that is still connecting was ignored and
    /// the station started playing under a deck that said it was paused.
    paused: Arc<AtomicBool>,
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
        let paused = Arc::new(AtomicBool::new(false));
        let thread_paused = Arc::clone(&paused);
        std::thread::Builder::new()
            .name("spot-radio".to_string())
            .spawn(move || audio_thread(rx, thread_tap, thread_generation, thread_paused))
            .expect("failed to spawn the radio audio thread");
        // Only the first player ever registers, which is the only one there
        // is: `Client` builds exactly one and holds it for the session.
        let _ = SHUTDOWN_HOOK.set(tx.clone());
        Self {
            tx,
            title: Arc::new(Mutex::new(None)),
            tap,
            generation,
            live: Arc::new(AtomicBool::new(false)),
            paused,
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

    /// Connect to `url`, decode it, and start playing it.
    ///
    /// Returns once the first audio is heard — so the caller can report a
    /// failure to the user instead of leaving them looking at a station that
    /// is silently not playing. That takes seconds, so the caller must be a
    /// task of its own and never a loop that has to answer a key in the
    /// meantime.
    ///
    /// The connect and the decoder are both built here, on the caller's task
    /// and on a blocking-pool thread, and only the finished source goes to the
    /// audio thread. Building the decoder reads the station's first frames, so
    /// doing it on the audio thread — which is what spot used to do — put a
    /// network read in the middle of the loop that answers pause and stop.
    pub async fn play(&self, url: &str, volume_percent: u8) -> Result<()> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.title.lock() = None;
        // A new station starts playing. Cleared here rather than on the
        // hand-off so that a pause pressed while this one connects is still
        // seen — the audio thread reads the flag when the stream arrives.
        self.paused.store(false, Ordering::SeqCst);
        self.tap.clear();

        let reader = self.open(url).await?;
        // Opening took as long as it took; the user may have chosen something
        // else in the meantime.
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }

        // `spawn_blocking`, because this reads the network and symphonia's
        // probe gives no promise about how much it reads. On the worst kind of
        // station it stops only when [`READ_TIMEOUT`] fires; a blocking-pool
        // thread can wait that out, the audio thread cannot.
        let tap = Arc::clone(&self.tap);
        let source = tokio::task::spawn_blocking(move || decode(reader, tap))
            .await
            .context("the radio decoder thread panicked")??;
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }

        // Marked live with the hand-off, not with the request: until the
        // source reaches the thread there is nothing playing to stop.
        self.live.store(true, Ordering::SeqCst);
        self.send(AudioCmd::Play {
            source: Box::new(source),
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
        //
        // The timeouts are what keep a bad station from being permanent. See
        // [`READ_TIMEOUT`]: without one, a server that goes quiet leaves a
        // reader blocked with nothing to wake it.
        let client = IcyClient::builder()
            .request_icy_metadata()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
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

    /// Silence the stream, keeping the station.
    ///
    /// The flag is set as well as the command sent, because a station that is
    /// still connecting has no sink to pause — see [`Self::paused`].
    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.send(AudioCmd::Pause);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
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

    /// Close the device and end the audio thread, and do not return until it
    /// has happened.
    ///
    /// [`Self::stop`] is not enough on the way out: it queues a command and
    /// returns, and spot's quit path used to reach the terminal restore — and
    /// the end of `main` — while a station was still streaming. This blocks on
    /// the thread's acknowledgement so quitting is audibly silent before the
    /// UI goes away.
    pub fn shutdown(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.live.store(false, Ordering::SeqCst);
        *self.title.lock() = None;
        let (ack_tx, ack_rx) = channel();
        self.send(AudioCmd::Shutdown(ack_tx));
        if ack_rx.recv_timeout(SHUTDOWN_ACK_TIMEOUT).is_err() {
            log::warn!("the radio audio thread did not confirm shutdown in time");
        }
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

/// Silence the radio thread from anywhere, without a [`RadioPlayer`] handle.
///
/// For the Windows console-control handler, which gets a few seconds' notice
/// that the window is closing and has no way to reach the client task. Does
/// nothing before the client is built, or once the thread has already been
/// shut down by the ordinary quit path.
pub fn stop_all_audio() {
    let Some(tx) = SHUTDOWN_HOOK.get() else {
        return;
    };
    let (ack_tx, ack_rx) = channel();
    if tx.send(AudioCmd::Shutdown(ack_tx)).is_ok() {
        let _ = ack_rx.recv_timeout(SHUTDOWN_ACK_TIMEOUT);
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
///
/// Nothing in this loop may block on the network. It is the only thread that
/// can silence the radio, and every command behind a blocked one is a control
/// that does nothing until the block clears — which, before the timeouts and
/// the move of the decoder off this thread, could be never.
fn audio_thread(
    rx: Receiver<AudioCmd>,
    tap: Arc<AudioTap>,
    generation: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
) {
    // Opened on the first Play and dropped on Stop, so the device is held only
    // while it is in use.
    let mut out: Option<(OutputStream, Sink)> = None;

    while let Ok(cmd) = rx.recv() {
        match cmd {
            AudioCmd::Play {
                source,
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
                if let Err(e) = start(&mut out, *source, volume, &paused) {
                    log::error!("radio playback failed: {e:#}");
                    silence(&mut out);
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
                silence(&mut out);
                tap.clear();
            }
            AudioCmd::SetVolume(v) => {
                if let Some((_, sink)) = &out {
                    sink.set_volume(v);
                }
            }
            AudioCmd::Shutdown(ack) => {
                // Dropped before the ack, not after: the sender's contract is
                // that the device is closed by the time it hears back.
                silence(&mut out);
                tap.clear();
                let _ = ack.send(());
                return;
            }
        }
    }
}

/// Build the decoder for a connected station.
///
/// Blocking, and unbounded but for [`READ_TIMEOUT`]: identifying the codec
/// means reading the first frames, and how long that takes is the station's
/// business. Runs on a blocking-pool thread — never on the audio thread.
fn decode(reader: RadioReader, tap: Arc<AudioTap>) -> Result<RadioSource> {
    let decoder = rodio::Decoder::builder()
        .with_data(reader)
        // A broadcast has no beginning to seek back to. Saying so stops
        // symphonia looking for a seek index that cannot exist.
        .with_seekable(false)
        .build()
        .context("could not decode the station's audio")?;
    Ok(TapSource::new(decoder, tap))
}

/// Silence the output and release the device.
///
/// The sink goes first and the stream second. Dropping the sink only sets a
/// flag, so it always returns at once; closing the device waits on the audio
/// backend, which is the slower of the two. In that order the radio is silent
/// before anything that can wait, so a stalled backend cannot keep the sound
/// on.
fn silence(out: &mut Option<(OutputStream, Sink)>) {
    if let Some((stream, sink)) = out.take() {
        drop(sink);
        drop(stream);
    }
}

fn start(
    out: &mut Option<(OutputStream, Sink)>,
    source: RadioSource,
    volume: f32,
    paused: &Arc<AtomicBool>,
) -> Result<()> {
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
    let (stream, sink) = out.as_mut().expect("the output was just opened");

    // Whatever was playing goes now, not when the new source runs out — and it
    // goes by being replaced rather than by `Sink::clear`. `clear` waits for
    // the source it is discarding to end, and ending it means the mixer
    // polling it once more; a station whose reads have stalled is never polled
    // again and the wait never returns. Dropping the old sink asks nothing of
    // the mixer: the source is marked stopped and collected in its own time.
    *sink = Sink::connect_new(stream.mixer());
    sink.set_volume(volume);
    sink.append(source);
    // A pause pressed while this station was connecting had no sink to act on;
    // this is where it lands.
    if paused.load(Ordering::SeqCst) {
        sink.pause();
    } else {
        sink.play();
    }
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

    /// A station takes seconds to connect, and the pause key works throughout
    /// them. The audio thread has no sink to pause over that window, so the
    /// intent has to be kept somewhere it can read when the stream lands —
    /// otherwise the station starts playing under a deck that says paused.
    #[test]
    fn a_pause_before_the_stream_lands_is_remembered() {
        let player = RadioPlayer::new(Arc::new(AudioTap::new()));
        assert!(
            !player.paused.load(Ordering::SeqCst),
            "a fresh player plays"
        );

        player.pause();
        assert!(player.paused.load(Ordering::SeqCst));

        player.resume();
        assert!(!player.paused.load(Ordering::SeqCst));
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
