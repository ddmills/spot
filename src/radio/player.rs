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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
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

/// Assumed bitrate, in kilobits, when the server does not report one. It sizes
/// the prefetch and nothing else.
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
/// enough in a directory of ten thousand stations — blocks the reading thread
/// for ever without it. That thread is the decoder's, and a decoder thread
/// that never returns is a radio that cannot be paused, stopped, or changed.
/// Per-read rather than a total timeout: a broadcast never ends, so
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
    /// Silence the stream but keep the device.
    ///
    /// What a station change wants. [`AudioCmd::Stop`] closes the output, so
    /// the station replacing this one pays a device open before its first
    /// sample, and on a stalled backend the close itself waits.
    Hush,
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
    /// The generation of the stream handed to the audio thread; 0 for none.
    ///
    /// The engine's own liveness, deliberately not read off `AppState.radio`.
    /// That field is a UI fact and this is an audio one, and asking the UI
    /// whether to stop the audio is what let both engines play at once.
    ///
    /// A generation rather than a flag so the mark cannot outlive its own
    /// station: a stop landing between the hand-off's last check and this
    /// store used to leave a `true` behind with nothing playing, and every
    /// transport key then routed to a station that was not there. Written
    /// stale, it now simply reads as stale. See [`Self::is_live`].
    live: Arc<AtomicU64>,
    /// Whether the user has asked for silence, shared with the audio thread.
    ///
    /// A station takes seconds to connect, and the pause key works throughout
    /// them — the deck is already drawn and already says the station is on. The
    /// audio thread has no sink to pause over that window, so the intent is
    /// recorded here instead and read when the stream finally arrives.
    /// Without it, pausing a station that is still connecting was ignored and
    /// the station started playing under a deck that said it was paused.
    paused: Arc<AtomicBool>,
    /// The generation of the last stream whose decoder ran dry.
    ///
    /// A broadcast has no length to reach, so nothing else in the app sees one
    /// end: the server closes, the decoder returns `None`, the sink drains to
    /// silence, and the deck goes on saying the station is on. This is the one
    /// place that notices — written by the source itself, on the audio
    /// callback thread, which is why it is an atomic and not a message.
    ///
    /// A generation rather than a flag so an old stream running out cannot
    /// condemn the station that replaced it. See [`Self::stream_ended`].
    ended: Arc<AtomicU64>,
    /// How many channels the live stream decodes to, or 0 when nothing is
    /// playing yet.
    ///
    /// The directory reports a codec and a bitrate but never a channel count,
    /// so the decoder is the only thing that knows whether a station is stereo
    /// or mono. Shared like [`Self::title`] so the deck reads it every frame
    /// without going through the client.
    channels: Arc<AtomicU8>,
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
            live: Arc::new(AtomicU64::new(0)),
            paused,
            ended: Arc::new(AtomicU64::new(0)),
            channels: Arc::new(AtomicU8::new(0)),
        }
    }

    /// Whether this thread is streaming a station.
    ///
    /// True from the moment a connected stream is handed to the audio thread
    /// until [`Self::stop`] or [`Self::hush`]. See [`Self::live`] for why the
    /// caller must ask here rather than looking at `AppState.radio`, and why
    /// the answer is a generation comparison rather than a flag.
    pub fn is_live(&self) -> bool {
        let live = self.live.load(Ordering::SeqCst);
        live != 0 && live == self.generation.load(Ordering::SeqCst)
    }

    /// The shared now-playing slot. Cloned into `AppState` so the deck can read
    /// it without going through the client.
    pub fn title(&self) -> Arc<Mutex<Option<String>>> {
        Arc::clone(&self.title)
    }

    /// The shared channel-count slot, cloned into `AppState` beside
    /// [`Self::title`] and read the same way.
    pub fn channels(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.channels)
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
    /// doing it on the audio thread would put a network read in the middle of
    /// the loop that answers pause and stop.
    pub async fn play(&self, url: &str, volume_percent: u8) -> Result<()> {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        *self.title.lock() = None;
        self.channels.store(0, Ordering::SeqCst);
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
        let ended = Arc::clone(&self.ended);
        let stamp = Arc::clone(&self.generation);
        let source = tokio::task::spawn_blocking(move || decode(reader, tap, ended, stamp))
            .await
            .context("the radio decoder thread panicked")??;
        if self.generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }

        // The decoder has read enough of the stream to know its layout, and
        // this is the last point at which the station is still the one asked
        // for.
        self.channels.store(
            source.channels().min(u8::MAX.into()) as u8,
            Ordering::SeqCst,
        );

        // Marked live with the hand-off, not with the request: until the
        // source reaches the thread there is nothing playing to stop. Marked
        // with this station's own generation, so a stop landing between the
        // check above and this store writes a mark that is already stale —
        // which is what `is_live` reads it as.
        self.live.store(generation, Ordering::SeqCst);
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
        self.live.store(0, Ordering::SeqCst);
        *self.title.lock() = None;
        self.channels.store(0, Ordering::SeqCst);
        self.send(AudioCmd::Stop);
    }

    /// Silence the stream at once, keeping the device open.
    ///
    /// What a station change asks for. Connecting the next station takes
    /// seconds — a directory address, a stream to reach, five seconds of
    /// prefetch — and without this the station being left goes on playing
    /// through all of them, under a deck that has already moved on.
    ///
    /// [`Self::stop`] would do it, but it also closes the output: the station
    /// arriving would pay a device open before its first sample, and closing
    /// waits on the audio backend, which a station whose reads have stalled
    /// can hold. Bumps the generation like `stop`, so a connect still in
    /// flight never reaches the sink either.
    pub fn hush(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.live.store(0, Ordering::SeqCst);
        *self.title.lock() = None;
        self.channels.store(0, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        // Here as well as on the audio thread: the tap is what the header
        // reads to tell a station that is playing from one that is still
        // arriving, and clearing it on the caller's thread is what flips the
        // corner to `LOADING` on the keypress rather than a second later.
        self.tap.clear();
        self.send(AudioCmd::Hush);
    }

    /// Whether the stream that is playing has run out.
    ///
    /// The station is still live as far as the engine is concerned — the
    /// device is open and the sink is connected — but the source behind it has
    /// nothing left to give, so what is coming out is silence. Only true for
    /// the stream now playing: an older one running out is stamped with its own
    /// generation and read as the history it is.
    pub fn stream_ended(&self) -> bool {
        let generation = self.generation.load(Ordering::SeqCst);
        generation != 0 && self.ended.load(Ordering::SeqCst) == generation
    }

    pub fn set_volume(&self, percent: u8) {
        self.send(AudioCmd::SetVolume(amplitude(percent)));
    }

    /// Close the device and end the audio thread, and do not return until it
    /// has happened.
    ///
    /// [`Self::stop`] is not enough on the way out: it queues a command and
    /// returns, which lets the quit path reach the terminal restore — and the
    /// end of `main` — while a station still streams. This blocks on the
    /// thread's acknowledgement so quitting is audibly silent before the UI
    /// goes away.
    pub fn shutdown(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.live.store(0, Ordering::SeqCst);
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
            // Replaced rather than cleared, for the reason `start` replaces it:
            // `Sink::clear` waits for the source it discards to end, and a
            // station whose reads have stalled is never polled again.
            AudioCmd::Hush => {
                if let Some((stream, sink)) = out.as_mut() {
                    *sink = Sink::connect_new(stream.mixer());
                }
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
fn decode(
    reader: RadioReader,
    tap: Arc<AudioTap>,
    ended: Arc<AtomicU64>,
    generation: Arc<AtomicU64>,
) -> Result<RadioSource> {
    let decoder = rodio::Decoder::builder()
        .with_data(reader)
        // A broadcast has no beginning to seek back to. Saying so stops
        // symphonia looking for a seek index that cannot exist.
        .with_seekable(false)
        .build()
        .context("could not decode the station's audio")?;
    Ok(TapSource::new(decoder, tap, ended, generation))
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
    /// Where the end of the broadcast is recorded. See [`RadioPlayer::ended`].
    ended: Arc<AtomicU64>,
    /// The engine's current generation, and the one this source was built for.
    ///
    /// Read on the audio callback thread so a source the user has moved on
    /// from ends itself at the next poll. The sink is replaced on a hush,
    /// which is what silences it, but the source is only *collected* when the
    /// mixer next asks it for a sample — and asking means another read on a
    /// station that may have stalled. Ending first is how it never asks.
    generation: Arc<AtomicU64>,
    stamp: u64,
}

impl<S: Source> TapSource<S> {
    fn new(
        inner: S,
        tap: Arc<AudioTap>,
        ended: Arc<AtomicU64>,
        generation: Arc<AtomicU64>,
    ) -> Self {
        let mono = inner.channels() == 1;
        let stamp = generation.load(Ordering::SeqCst);
        Self {
            inner,
            tap,
            buf: Vec::with_capacity(TAP_FLUSH),
            mono,
            ended,
            generation,
            stamp,
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
        // The user has moved on. Ending here rather than being asked for one
        // more sample is what keeps a stalled station from taking another
        // fifteen-second read with the mixer waiting on it.
        if self.stamp != self.generation.load(Ordering::SeqCst) {
            return None;
        }
        let Some(sample) = self.inner.next() else {
            // The server closed, or a read timed out and ended the download.
            // A broadcast has no length to reach, so nothing else in the app
            // sees one end — the sink simply drains to silence under a deck
            // that goes on saying the station is on. This is where it is
            // recorded, stamped with the stream it belongs to.
            self.ended.store(self.stamp, Ordering::SeqCst);
            return None;
        };
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
        hand_off(&player);
        assert!(player.is_live());

        player.stop();
        assert!(!player.is_live(), "stop is what ends it");
    }

    /// The mark is the generation it was made for, so a stop that lands after
    /// the hand-off's last check cannot leave the engine claiming a station
    /// the audio thread has already dropped — every transport key routed to
    /// that station, and nothing could stop it.
    #[test]
    fn a_stop_racing_the_handoff_leaves_no_station_behind() {
        let player = RadioPlayer::new(Arc::new(AudioTap::new()));
        let generation = player.generation.fetch_add(1, Ordering::SeqCst) + 1;
        player.stop();
        player.live.store(generation, Ordering::SeqCst);
        assert!(!player.is_live());
    }

    /// A station change silences the stream and keeps the device: the next
    /// station pays no reopen, and nothing waits on a backend a stalled
    /// station can hold.
    #[test]
    fn a_hush_ends_the_stream_without_ending_the_session() {
        let player = RadioPlayer::new(Arc::new(AudioTap::new()));
        hand_off(&player);
        *player.title.lock() = Some("Aspen — Seasick".into());
        player.pause();

        player.hush();
        assert!(!player.is_live());
        assert!(
            player.title.lock().is_none(),
            "the last title is not this one"
        );
        assert!(
            !player.paused.load(Ordering::SeqCst),
            "a pause is about a station, and this one is gone"
        );
    }

    /// A broadcast has no length to reach, so a decoder running dry is the only
    /// end there is. It is read against the generation it belongs to: a station
    /// the user has already left must not condemn the one that replaced it.
    #[test]
    fn a_stream_that_runs_dry_is_read_against_its_own_generation() {
        let player = RadioPlayer::new(Arc::new(AudioTap::new()));
        let generation = hand_off(&player);
        assert!(!player.stream_ended(), "nothing has run out");

        player.ended.store(generation, Ordering::SeqCst);
        assert!(player.stream_ended());

        player.hush();
        assert!(!player.stream_ended(), "that was the station before this");
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

    /// A source under test, on a generation nothing moves off.
    fn tapped(inner: Tone, tap: &Arc<AudioTap>) -> TapSource<Tone> {
        TapSource::new(
            inner,
            Arc::clone(tap),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(1)),
        )
    }

    /// Stand in for a connected stream reaching the audio thread, which `play`
    /// needs a network and a device to do. Returns the generation it took.
    fn hand_off(player: &RadioPlayer) -> u64 {
        let generation = player.generation.fetch_add(1, Ordering::SeqCst) + 1;
        player.live.store(generation, Ordering::SeqCst);
        generation
    }

    /// A broadcast has no length to reach, so a server that closes is the only
    /// end there is — and the source is the one thing that sees it. The stamp
    /// is what keeps an old stream running dry from condemning the station
    /// that replaced it.
    #[test]
    fn a_stream_that_runs_dry_is_recorded_against_its_own_generation() {
        let tap = Arc::new(AudioTap::new());
        let ended = Arc::new(AtomicU64::new(0));
        let generation = Arc::new(AtomicU64::new(7));
        let mut src = TapSource::new(
            tone(vec![0.5], 2),
            Arc::clone(&tap),
            Arc::clone(&ended),
            Arc::clone(&generation),
        );

        assert_eq!(src.next(), Some(0.5));
        assert_eq!(ended.load(Ordering::SeqCst), 0, "not ended while sending");
        assert_eq!(src.next(), None);
        assert_eq!(ended.load(Ordering::SeqCst), 7);
    }

    /// A source the user has moved off ends at its next poll rather than being
    /// asked for one more sample. Asking means another read, and a station
    /// whose reads have stalled would hold the mixer on it for the length of
    /// the read timeout — with the station that replaced it waiting behind.
    #[test]
    fn a_source_the_user_has_left_ends_rather_than_reading_again() {
        let tap = Arc::new(AudioTap::new());
        let ended = Arc::new(AtomicU64::new(0));
        let generation = Arc::new(AtomicU64::new(3));
        let mut src = TapSource::new(
            tone(vec![0.5; 8], 2),
            Arc::clone(&tap),
            Arc::clone(&ended),
            Arc::clone(&generation),
        );

        assert_eq!(src.next(), Some(0.5));
        generation.store(4, Ordering::SeqCst);
        assert_eq!(src.next(), None);
        assert_eq!(
            ended.load(Ordering::SeqCst),
            0,
            "a station left behind has not run out; it has been left behind"
        );
    }

    #[test]
    fn tap_source_passes_samples_through_unchanged() {
        let tap = Arc::new(AudioTap::new());
        let src = tapped(tone(vec![0.25, -0.25, 1.0, -1.0], 2), &tap);
        assert_eq!(src.collect::<Vec<_>>(), vec![0.25, -0.25, 1.0, -1.0]);
    }

    #[test]
    fn tap_source_fills_the_tap() {
        let tap = Arc::new(AudioTap::new());
        // Enough samples to cross the flush threshold; the tap only sees
        // whole flushes.
        let samples: Vec<f32> = (0..TAP_FLUSH).map(|_| 0.5).collect();
        let mut src = tapped(tone(samples, 2), &tap);
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
        let mut src = tapped(tone(samples, 1), &tap);
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
