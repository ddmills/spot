//! spot's audio backend for librespot.
//!
//! librespot ships a rodio backend already, but its `stop` is
//! `sleep_until_end()` followed by `pause()` — pausing plays the whole queued
//! half second out first, so the audio you just stopped keeps going. This is
//! the same sink with the opposite stop: fade to zero over a few milliseconds,
//! pause, then drop the queue, so pause is silent essentially at once.
//!
//! The device is opened the way `radio::player` opens its own, which is the
//! only other place in spot that talks to rodio directly.

use std::thread;
use std::time::Duration;

use librespot_playback::audio_backend::{Sink, SinkError, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::{NUM_CHANNELS, SAMPLE_RATE};
use rodio::{OutputStream, OutputStreamBuilder};

/// Steps in the pause ramp. rodio applies a volume change in its 5 ms
/// `periodic_access` tick, so there is no point asking for a finer ramp than
/// this.
const FADE_STEPS: u32 = 4;
const FADE_STEP: Duration = Duration::from_millis(3);

/// Queued chunks to keep ahead of the device, matching librespot's own rodio
/// backend. Chunks run 256-3000 samples, so this is roughly half a second —
/// enough that a stalled decoder does not glitch, and no longer a latency
/// problem now that pause drops the queue instead of playing it out.
const QUEUE_HIGH_WATER: usize = 26;

/// How long to wait for rodio to drain when the queue is full.
const DRAIN_POLL: Duration = Duration::from_millis(10);

pub struct SpotSink {
    sink: rodio::Sink,
    /// Held for as long as the sink is: dropping it closes the device.
    _stream: OutputStream,
}

impl SpotSink {
    pub fn open() -> Result<Self, SinkError> {
        let mut stream = OutputStreamBuilder::open_default_stream().map_err(|e| {
            SinkError::ConnectionRefused(format!("could not open an audio output device: {e}"))
        })?;
        // rodio's `Drop` otherwise prints a line about the stream closing
        // straight into the alternate screen, over whatever the UI had drawn.
        stream.log_on_drop(false);
        let sink = rodio::Sink::connect_new(stream.mixer());
        Ok(Self {
            sink,
            _stream: stream,
        })
    }
}

impl Sink for SpotSink {
    fn start(&mut self) -> SinkResult<()> {
        // Undo a fade that `stop` left behind, including one interrupted by a
        // resume landing mid-ramp.
        self.sink.set_volume(1.0);
        self.sink.play();
        Ok(())
    }

    /// librespot stops the sink on pause, seek and track end.
    ///
    /// The ramp is only there to stop a cut mid-waveform clicking; `pause` is
    /// what actually silences the output, and `clear` throws away the queued
    /// audio so a resume picks up where the player says it should rather than
    /// replaying what was buffered. `clear` drains at one chunk per rodio tick,
    /// which takes a moment — but the sink is already paused, so none of it is
    /// audible.
    fn stop(&mut self) -> SinkResult<()> {
        for step in (0..FADE_STEPS).rev() {
            self.sink.set_volume(step as f32 / FADE_STEPS as f32);
            thread::sleep(FADE_STEP);
        }
        self.sink.pause();
        self.sink.clear();
        self.sink.set_volume(1.0);
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let samples_f32: &[f32] = &converter.f64_to_f32(samples);
        self.sink.append(rodio::buffer::SamplesBuffer::new(
            NUM_CHANNELS as u16,
            SAMPLE_RATE,
            samples_f32,
        ));

        // Back-pressure: librespot hands over packets as fast as it can decode
        // them, so without this the queue grows without bound.
        while self.sink.len() > QUEUE_HIGH_WATER {
            thread::sleep(DRAIN_POLL);
        }
        Ok(())
    }
}
