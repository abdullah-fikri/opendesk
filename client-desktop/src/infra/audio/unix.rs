//! Voice devices through cpal (ALSA; PipeWire and PulseAudio serve the ALSA `default` device). The microphone
//! opens only while a session is active; playback opens on the first packet and closes after 2 s of silence.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, FromSample, SampleRate, SizedSample, Stream, StreamConfig};

use super::{VoiceDspState, quality_to_audio_params};
use crate::infra::logger;
use crate::protocol::Outgoing;

/// Mono/stereo and sample formats tried in order; devices without mono or i16 get converted.
const LAYOUTS: [(u16, bool); 4] = [(1, false), (1, true), (2, false), (2, true)];

fn config(rate: u32, channels: u16) -> StreamConfig {
    StreamConfig { channels, sample_rate: SampleRate(rate), buffer_size: BufferSize::Default }
}

fn build_input<T>(dev: &Device, rate: u32, channels: u16, tx: Sender<Vec<i16>>) -> Result<Stream, String>
where
    T: SizedSample,
    i16: FromSample<T>,
{
    dev.build_input_stream(
        &config(rate, channels),
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mono: Vec<i16> = data.chunks(channels as usize).map(|f| f[0].to_sample::<i16>()).collect();
            let _ = tx.send(mono);
        },
        |e| logger::warn("desk_rt.audio_in", &format!("Microphone stream error: {e}")),
        None,
    )
    .map_err(|e| e.to_string())
}

fn open_input(rate: u32, tx: &Sender<Vec<i16>>) -> Result<Stream, String> {
    let dev = cpal::default_host().default_input_device().ok_or("no microphone")?;
    let mut last = String::new();
    for (channels, float) in LAYOUTS {
        let built = if float {
            build_input::<f32>(&dev, rate, channels, tx.clone())
        } else {
            build_input::<i16>(&dev, rate, channels, tx.clone())
        };
        match built {
            Ok(s) => return s.play().map(|_| s).map_err(|e| e.to_string()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Samples waiting for the speaker, with a jitter prebuffer.
struct Playback {
    queue: VecDeque<i16>,
    primed: bool,
    prebuffer: usize,
}

fn build_output<T>(dev: &Device, rate: u32, channels: u16, ring: Arc<Mutex<Playback>>) -> Result<Stream, String>
where
    T: SizedSample + FromSample<i16>,
{
    dev.build_output_stream(
        &config(rate, channels),
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            let mut p = ring.lock().unwrap_or_else(|e| e.into_inner());
            if !p.primed && p.queue.len() >= p.prebuffer {
                p.primed = true;
            }
            for frame in out.chunks_mut(channels as usize) {
                let s = if p.primed {
                    match p.queue.pop_front() {
                        Some(s) => s,
                        None => {
                            // Underrun: refill the prebuffer before playing again.
                            p.primed = false;
                            0
                        }
                    }
                } else {
                    0
                };
                frame.fill(T::from_sample(s));
            }
        },
        |e| logger::warn("desk_rt.audio_out", &format!("Speaker stream error: {e}")),
        None,
    )
    .map_err(|e| e.to_string())
}

fn open_output(rate: u32, ring: &Arc<Mutex<Playback>>) -> Result<Stream, String> {
    let dev = cpal::default_host().default_output_device().ok_or("no speaker")?;
    let mut last = String::new();
    for (channels, float) in LAYOUTS {
        let built = if float {
            build_output::<f32>(&dev, rate, channels, Arc::clone(ring))
        } else {
            build_output::<i16>(&dev, rate, channels, Arc::clone(ring))
        };
        match built {
            Ok(s) => return s.play().map(|_| s).map_err(|e| e.to_string()),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Two-way voice for the current session: microphone capture into binary packets and playback.
pub struct VoiceEngine {
    tx_play: Sender<(u32, Vec<u8>)>,
    pub is_session_active: Arc<AtomicBool>,
    pub is_mic_muted: Arc<AtomicBool>,
    pub audio_quality: Arc<AtomicUsize>,
}

impl VoiceEngine {
    /// Starts the capture and playback threads. Devices open only while `is_session_active` is set; captured packets go to `tx_out`.
    pub fn new(tx_out: Sender<Outgoing>) -> Arc<Self> {
        let (tx_play, rx_play) = mpsc::channel::<(u32, Vec<u8>)>();
        let is_session_active = Arc::new(AtomicBool::new(false));
        let is_mic_muted = Arc::new(AtomicBool::new(false));
        // Default to tier 2 (48 kHz), as on Windows.
        let audio_quality = Arc::new(AtomicUsize::new(2));

        let (active, muted, quality) =
            (Arc::clone(&is_session_active), Arc::clone(&is_mic_muted), Arc::clone(&audio_quality));
        thread::spawn(move || {
            let mut warned = false;
            loop {
                while !active.load(Ordering::Relaxed) {
                    warned = false;
                    thread::sleep(Duration::from_millis(25));
                }
                let tier = quality.load(Ordering::Relaxed);
                let (rate, tag, frame_bytes) = quality_to_audio_params(tier);
                let (tx_pcm, rx_pcm) = mpsc::channel::<Vec<i16>>();
                let stream = match open_input(rate, &tx_pcm) {
                    Ok(s) => s,
                    Err(e) => {
                        if !warned {
                            logger::warn("desk_rt.audio_in", &format!("Microphone unavailable ({rate} Hz): {e}"));
                            warned = true;
                        }
                        thread::sleep(Duration::from_secs(3));
                        continue;
                    }
                };
                drop(tx_pcm);
                logger::info("desk_rt.audio_in", &format!("Microphone capture opened at {rate} Hz."));
                let mut dsp = VoiceDspState::new();
                let mut pending: Vec<u8> = Vec::with_capacity(frame_bytes * 2);
                let mut processed = vec![0u8; frame_bytes];
                while active.load(Ordering::Relaxed) && quality.load(Ordering::Relaxed) == tier {
                    match rx_pcm.recv_timeout(Duration::from_millis(100)) {
                        Ok(samples) => {
                            pending.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
                            while pending.len() >= frame_bytes {
                                if !muted.load(Ordering::Relaxed) {
                                    dsp.process_pcm_16le(&pending[..frame_bytes], &mut processed);
                                    let mut pkt = Vec::with_capacity(2 + frame_bytes);
                                    pkt.extend_from_slice(&[0x02, tag]);
                                    pkt.extend_from_slice(&processed);
                                    let _ = tx_out.send(Outgoing::Audio(pkt));
                                }
                                pending.drain(..frame_bytes);
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                drop(stream);
                logger::info("desk_rt.audio_in", "Microphone capture device closed.");
            }
        });

        thread::spawn(move || {
            let ring = Arc::new(Mutex::new(Playback { queue: VecDeque::new(), primed: false, prebuffer: 0 }));
            let mut stream: Option<(u32, Stream)> = None;
            let mut failed_rate = 0;
            loop {
                match rx_play.recv_timeout(Duration::from_secs(2)) {
                    Ok((rate, pcm)) => {
                        if stream.as_ref().is_none_or(|(r, _)| *r != rate) {
                            stream = None;
                            if let Ok(mut p) = ring.lock() {
                                // 60 ms prebuffer absorbs network jitter.
                                *p = Playback {
                                    queue: VecDeque::new(),
                                    primed: false,
                                    prebuffer: rate as usize * 60 / 1000,
                                };
                            }
                            match open_output(rate, &ring) {
                                Ok(s) => stream = Some((rate, s)),
                                Err(e) if failed_rate != rate => {
                                    logger::warn("desk_rt.audio_out", &format!("Speaker unavailable ({rate} Hz): {e}"));
                                    failed_rate = rate;
                                }
                                Err(_) => {}
                            }
                        }
                        if stream.is_some()
                            && let Ok(mut p) = ring.lock()
                        {
                            p.queue.extend(pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)));
                            // Never fall more than 400 ms behind.
                            let cap = rate as usize * 2 / 5;
                            if p.queue.len() > cap {
                                let excess = p.queue.len() - cap;
                                p.queue.drain(..excess);
                            }
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        stream = None;
                        failed_rate = 0;
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        });

        Arc::new(Self { tx_play, is_session_active, is_mic_muted, audio_quality })
    }

    /// Plays incoming binary audio frame (`[0x02, rate_khz, pcm...]` or legacy `[0x02, pcm...]`)
    pub fn play_binary_packet(&self, bin: &[u8]) {
        if !self.is_session_active.load(Ordering::Relaxed) || bin.len() <= 2 {
            return;
        }
        // Even total length with valid rate_khz tag (16, 24, 48) -> new header format
        let (rate_hz, pcm_slice) = if bin.len().is_multiple_of(2) && (bin[1] == 16 || bin[1] == 24 || bin[1] == 48) {
            ((bin[1] as u32) * 1000, &bin[2..])
        } else {
            (16000, &bin[1..])
        };
        if !pcm_slice.is_empty() {
            let _ = self.tx_play.send((rate_hz, pcm_slice.to_vec()));
        }
    }
}
