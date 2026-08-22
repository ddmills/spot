//! PCM tap on the librespot audio pipeline, feeding the player view's
//! spectrum visualizer. `TapSink` wraps the real audio backend, copying
//! samples into a shared ring buffer on their way to the device.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use librespot_playback::audio_backend::{Sink, SinkResult};
use librespot_playback::convert::Converter;
use librespot_playback::decoder::AudioPacket;
use librespot_playback::mixer::VolumeGetter;
use parking_lot::Mutex;

/// Mono samples retained for analysis (~185 ms at 44.1 kHz).
const RING_CAP: usize = 8192;

/// Shared sample buffer plus a freshness clock. The audio thread pushes,
/// the draw loop reads; neither ever holds this lock while touching
/// `AppState`, so there is no ordering hazard with the UI lock.
pub struct AudioTap {
    ring: Mutex<VecDeque<f32>>,
    /// Reference point for `last_write_ms` (an `Instant` can't live in an
    /// atomic, so writes store milliseconds since this epoch).
    epoch: Instant,
    /// Milliseconds after `epoch` of the most recent push; 0 = never/cleared.
    last_write_ms: AtomicU64,
}

impl AudioTap {
    pub fn new() -> Self {
        Self {
            ring: Mutex::new(VecDeque::with_capacity(RING_CAP)),
            epoch: Instant::now(),
            last_write_ms: AtomicU64::new(0),
        }
    }

    /// Append interleaved stereo samples, downmixed to mono and scaled by
    /// `gain` (used to undo the player's soft-volume attenuation, keeping
    /// the visualizer volume-agnostic).
    pub fn push(&self, samples: &[f64], gain: f64) {
        let mut ring = self.ring.lock();
        for pair in samples.chunks(2) {
            let mono = (gain * pair.iter().sum::<f64>() / pair.len() as f64) as f32;
            if ring.len() == RING_CAP {
                ring.pop_front();
            }
            ring.push_back(mono);
        }
        // .max(1): a push in the epoch's first millisecond must not read
        // back as "never written".
        let ms = (self.epoch.elapsed().as_millis() as u64).max(1);
        self.last_write_ms.store(ms, Ordering::Relaxed);
    }

    /// Copy the most recent `n` samples into `out` (oldest first).
    pub fn latest(&self, out: &mut Vec<f32>, n: usize) {
        out.clear();
        let ring = self.ring.lock();
        let skip = ring.len().saturating_sub(n);
        out.extend(ring.iter().skip(skip).copied());
    }

    /// Whether samples arrived within `within` (false after pause/stop or
    /// when another device is playing).
    pub fn is_fresh(&self, within: Duration) -> bool {
        let last = self.last_write_ms.load(Ordering::Relaxed);
        last != 0
            && self
                .epoch
                .elapsed()
                .saturating_sub(Duration::from_millis(last))
                <= within
    }

    pub fn clear(&self) {
        self.ring.lock().clear();
        self.last_write_ms.store(0, Ordering::Relaxed);
    }
}

/// Audio sink that tees samples into an [`AudioTap`] before delegating to
/// the real backend.
pub struct TapSink {
    inner: Box<dyn Sink>,
    tap: Arc<AudioTap>,
    /// The soft mixer's live attenuation, undone at tap time: samples reach
    /// the sink post-volume, but the visualizer should be volume-agnostic.
    volume: Box<dyn VolumeGetter + Send>,
}

impl TapSink {
    pub fn new(
        inner: Box<dyn Sink>,
        tap: Arc<AudioTap>,
        volume: Box<dyn VolumeGetter + Send>,
    ) -> Self {
        Self { inner, tap, volume }
    }
}

impl Sink for TapSink {
    fn start(&mut self) -> SinkResult<()> {
        self.inner.start()
    }

    /// librespot stops the sink on pause; clearing here makes the
    /// visualizer's staleness check flip immediately.
    fn stop(&mut self) -> SinkResult<()> {
        self.tap.clear();
        self.inner.stop()
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        // Raw (passthrough) packets aren't PCM; nothing useful to tap.
        if let AudioPacket::Samples(samples) = &packet {
            // Near-mute the signal is unrecoverable (0/0); push it as-is
            // and let the bars rest.
            let factor = self.volume.attenuation_factor();
            let gain = if factor > 1e-4 { 1.0 / factor } else { 1.0 };
            self.tap.push(samples, gain);
        }
        self.inner.write(packet, converter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_downmixes_stereo_to_mono() {
        let tap = AudioTap::new();
        tap.push(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 1.0);
        let mut out = Vec::new();
        tap.latest(&mut out, 10);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn push_applies_gain() {
        let tap = AudioTap::new();
        tap.push(&[0.25, 0.25], 2.0);
        let mut out = Vec::new();
        tap.latest(&mut out, 10);
        assert_eq!(out, vec![0.5]);
    }

    #[test]
    fn ring_caps_and_keeps_newest() {
        let tap = AudioTap::new();
        let stereo: Vec<f64> = (0..(RING_CAP as u64 + 10) * 2)
            .map(|i| (i / 2) as f64)
            .collect();
        tap.push(&stereo, 1.0);
        let mut out = Vec::new();
        tap.latest(&mut out, RING_CAP + 100);
        assert_eq!(out.len(), RING_CAP);
        assert_eq!(out[0], 10.0);
        assert_eq!(*out.last().unwrap(), (RING_CAP + 9) as f32);
    }

    #[test]
    fn latest_returns_tail_only() {
        let tap = AudioTap::new();
        tap.push(&[1.0, 1.0, 2.0, 2.0, 3.0, 3.0], 1.0);
        let mut out = Vec::new();
        tap.latest(&mut out, 2);
        assert_eq!(out, vec![2.0, 3.0]);
    }

    #[test]
    fn freshness_tracks_pushes_and_clear() {
        let tap = AudioTap::new();
        assert!(!tap.is_fresh(Duration::from_secs(60)));
        tap.push(&[0.0, 0.0], 1.0);
        assert!(tap.is_fresh(Duration::from_secs(60)));
        tap.clear();
        assert!(!tap.is_fresh(Duration::from_secs(60)));
        let mut out = Vec::new();
        tap.latest(&mut out, 10);
        assert!(out.is_empty());
    }
}
