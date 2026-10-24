//! Voice call audio through the Windows waveIn/waveOut API: captures the microphone into binary voice
//! packets (`[0x02][rate_khz][pcm16le]`) for the outbound queue and plays received packets. A light DSP stage
//! (high-pass, noise gate, soft limiter) runs on captured audio.

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use super::{VoiceDspState, quality_to_audio_params};
use crate::infra::logger;
use crate::protocol::Outgoing;

type HWAVEIN = *mut c_void;
type HWAVEOUT = *mut c_void;
type MMRESULT = u32;

const WAVE_MAPPER: u32 = 0xFFFFFFFF;
const WAVE_FORMAT_PCM: u16 = 1;
const CALLBACK_NULL: u32 = 0x00000000;
const WHDR_DONE: u32 = 0x00000001;
const WHDR_PREPARED: u32 = 0x00000002;
const WHDR_INQUEUE: u32 = 0x00000010;

const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;
const IN_RING_COUNT: usize = 12;
const OUT_RING_COUNT: usize = 24;
const MAX_FRAME_BYTES: usize = 3840; // Up to 40ms @ 48kHz 16-bit mono
const MAX_OUT_BUF: usize = 8192;

#[repr(C)]
struct WAVEFORMATEX {
    w_format_tag: u16,
    n_channels: u16,
    n_samples_per_sec: u32,
    n_avg_bytes_per_sec: u32,
    n_block_align: u16,
    w_bits_per_sample: u16,
    cb_size: u16,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct WAVEHDR {
    lp_data: *mut u8,
    dw_buffer_length: u32,
    dw_bytes_recorded: u32,
    dw_user: usize,
    dw_flags: u32,
    dw_loops: u32,
    lp_next: *mut WAVEHDR,
    reserved: usize,
}

unsafe impl Send for WAVEHDR {}

#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(u_period: u32) -> MMRESULT;
    fn waveInOpen(
        phwi: *mut HWAVEIN,
        u_device_id: u32,
        pwfx: *const WAVEFORMATEX,
        dw_callback: usize,
        dw_instance: usize,
        fdw_open: u32,
    ) -> MMRESULT;
    fn waveInPrepareHeader(hwi: HWAVEIN, pwh: *mut WAVEHDR, cbwh: u32) -> MMRESULT;
    fn waveInUnprepareHeader(hwi: HWAVEIN, pwh: *mut WAVEHDR, cbwh: u32) -> MMRESULT;
    fn waveInAddBuffer(hwi: HWAVEIN, pwh: *mut WAVEHDR, cbwh: u32) -> MMRESULT;
    fn waveInStart(hwi: HWAVEIN) -> MMRESULT;
    fn waveInReset(hwi: HWAVEIN) -> MMRESULT;
    fn waveInClose(hwi: HWAVEIN) -> MMRESULT;

    fn waveOutOpen(
        phwo: *mut HWAVEOUT,
        u_device_id: u32,
        pwfx: *const WAVEFORMATEX,
        dw_callback: usize,
        dw_instance: usize,
        fdw_open: u32,
    ) -> MMRESULT;
    fn waveOutPrepareHeader(hwo: HWAVEOUT, pwh: *mut WAVEHDR, cbwh: u32) -> MMRESULT;
    fn waveOutUnprepareHeader(hwo: HWAVEOUT, pwh: *mut WAVEHDR, cbwh: u32) -> MMRESULT;
    fn waveOutWrite(hwo: HWAVEOUT, pwh: *mut WAVEHDR, cbwh: u32) -> MMRESULT;
    fn waveOutPause(hwo: HWAVEOUT) -> MMRESULT;
    fn waveOutRestart(hwo: HWAVEOUT) -> MMRESULT;
    fn waveOutReset(hwo: HWAVEOUT) -> MMRESULT;
    fn waveOutClose(hwo: HWAVEOUT) -> MMRESULT;
    fn waveOutSetVolume(hwo: HWAVEOUT, dw_volume: u32) -> MMRESULT;
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
        unsafe {
            // Enable 1.0ms OS scheduler timer resolution for ultra-low network & audio latency
            timeBeginPeriod(1);
        }

        let (tx_play, rx_play) = mpsc::channel::<(u32, Vec<u8>)>();
        let is_session_active = Arc::new(AtomicBool::new(false));
        let is_mic_muted = Arc::new(AtomicBool::new(false));
        // Default to tier 2 (48kHz HD Voice like WhatsApp/AnyDesk)
        let audio_quality = Arc::new(AtomicUsize::new(2));

        let active_cap = Arc::clone(&is_session_active);
        let muted_cap = Arc::clone(&is_mic_muted);
        let quality_cap = Arc::clone(&audio_quality);

        // 1. Microphone Capture Thread
        // Keeps waveIn open across mute/unmute toggles during a session so re-enabling mic never fails!
        thread::spawn(move || unsafe {
            let block_align = (CHANNELS * BITS_PER_SAMPLE) / 8;
            let mut processed_buf = [0u8; MAX_FRAME_BYTES];

            loop {
                while !active_cap.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(25));
                }

                let cur_quality = quality_cap.load(Ordering::Relaxed);
                let (sample_rate, rate_tag_khz, frame_bytes) = quality_to_audio_params(cur_quality);

                let wfx = WAVEFORMATEX {
                    w_format_tag: WAVE_FORMAT_PCM,
                    n_channels: CHANNELS,
                    n_samples_per_sec: sample_rate,
                    n_avg_bytes_per_sec: sample_rate * block_align as u32,
                    n_block_align: block_align,
                    w_bits_per_sample: BITS_PER_SAMPLE,
                    cb_size: 0,
                };

                let mut h_in: HWAVEIN = ptr::null_mut();
                let res = waveInOpen(&mut h_in, WAVE_MAPPER, &wfx, 0, 0, CALLBACK_NULL);
                if res != 0 || h_in.is_null() {
                    logger::warn("desk_rt.audio_in", &format!("waveInOpen ({}Hz) failed: {}", sample_rate, res));
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                let mut buffers: Vec<Box<[u8; MAX_FRAME_BYTES]>> =
                    (0..IN_RING_COUNT).map(|_| Box::new([0u8; MAX_FRAME_BYTES])).collect();

                let mut headers: Box<[WAVEHDR; IN_RING_COUNT]> = Box::new(
                    [WAVEHDR {
                        lp_data: ptr::null_mut(),
                        dw_buffer_length: frame_bytes as u32,
                        dw_bytes_recorded: 0,
                        dw_user: 0,
                        dw_flags: 0,
                        dw_loops: 0,
                        lp_next: ptr::null_mut(),
                        reserved: 0,
                    }; IN_RING_COUNT],
                );
                let hdr_size = std::mem::size_of::<WAVEHDR>() as u32;
                let headers_ptr = headers.as_mut_ptr();

                for (i, buffer) in buffers.iter_mut().enumerate() {
                    let h_ptr = headers_ptr.add(i);
                    (*h_ptr).lp_data = buffer.as_mut_ptr();
                    (*h_ptr).dw_buffer_length = frame_bytes as u32;
                    (*h_ptr).dw_bytes_recorded = 0;
                    (*h_ptr).dw_flags = 0;
                    waveInPrepareHeader(h_in, h_ptr, hdr_size);
                    waveInAddBuffer(h_in, h_ptr, hdr_size);
                }

                waveInStart(h_in);
                logger::info(
                    "desk_rt.audio_in",
                    &format!("Microphone capture active ({}Hz Mono, 20ms frame).", sample_rate),
                );

                let mut dsp = VoiceDspState::new();
                let mut idx = 0usize;

                while active_cap.load(Ordering::Relaxed) && quality_cap.load(Ordering::Relaxed) == cur_quality {
                    let h_ptr = headers_ptr.add(idx);
                    // Volatile read is mandatory so LLVM --release never caches dw_flags!
                    let flags = ptr::read_volatile(&(*h_ptr).dw_flags);
                    if (flags & WHDR_DONE) != 0 {
                        let recorded = (ptr::read_volatile(&(*h_ptr).dw_bytes_recorded) as usize).min(frame_bytes) & !1;
                        if recorded > 0 && !muted_cap.load(Ordering::Relaxed) {
                            let slice = std::slice::from_raw_parts((*h_ptr).lp_data, recorded);
                            dsp.process_pcm_16le(slice, &mut processed_buf[..recorded]);
                            // Binary fast-path: [0x02, rate_khz, ...pcm_bytes] (even total length)
                            let mut pkt = Vec::with_capacity(2 + recorded);
                            pkt.push(0x02);
                            pkt.push(rate_tag_khz);
                            pkt.extend_from_slice(&processed_buf[..recorded]);
                            let _ = tx_out.send(Outgoing::Audio(pkt));
                        }
                        waveInUnprepareHeader(h_in, h_ptr, hdr_size);
                        (*h_ptr).dw_bytes_recorded = 0;
                        (*h_ptr).dw_flags = 0;
                        (*h_ptr).dw_buffer_length = frame_bytes as u32;
                        waveInPrepareHeader(h_in, h_ptr, hdr_size);
                        waveInAddBuffer(h_in, h_ptr, hdr_size);
                        idx = (idx + 1) % IN_RING_COUNT;
                    } else {
                        thread::sleep(Duration::from_millis(3));
                    }
                }

                waveInReset(h_in);
                for i in 0..IN_RING_COUNT {
                    let h_ptr = headers_ptr.add(i);
                    if (ptr::read_volatile(&(*h_ptr).dw_flags) & WHDR_PREPARED) != 0 {
                        waveInUnprepareHeader(h_in, h_ptr, hdr_size);
                    }
                }
                waveInClose(h_in);
                logger::info("desk_rt.audio_in", "Microphone capture device closed.");
            }
        });

        // 2. Speaker Playback Thread (Dynamic 16kHz / 24kHz / 48kHz waveOut with low-latency jitter prebuffer)
        thread::spawn(move || unsafe {
            let block_align = (CHANNELS * BITS_PER_SAMPLE) / 8;
            let mut h_out: HWAVEOUT = ptr::null_mut();
            let mut current_rate: u32 = 0;

            let mut buffers: Vec<Box<[u8; MAX_OUT_BUF]>> =
                (0..OUT_RING_COUNT).map(|_| Box::new([0u8; MAX_OUT_BUF])).collect();

            let mut headers: Box<[WAVEHDR; OUT_RING_COUNT]> = Box::new(
                [WAVEHDR {
                    lp_data: ptr::null_mut(),
                    dw_buffer_length: 0,
                    dw_bytes_recorded: 0,
                    dw_user: 0,
                    dw_flags: 0,
                    dw_loops: 0,
                    lp_next: ptr::null_mut(),
                    reserved: 0,
                }; OUT_RING_COUNT],
            );
            let hdr_size = std::mem::size_of::<WAVEHDR>() as u32;
            let headers_ptr = headers.as_mut_ptr();

            let mut next_slot = 0usize;
            let mut is_paused = false;
            let mut prebuf_count = 0usize;

            loop {
                let (pkt_rate, pcm) = match rx_play.recv_timeout(Duration::from_millis(45)) {
                    Ok(item) => item,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if is_paused && !h_out.is_null() && prebuf_count > 0 {
                            waveOutRestart(h_out);
                            is_paused = false;
                            prebuf_count = 0;
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
                if pcm.is_empty() {
                    continue;
                }

                // Reopen waveOut if sample rate changed (e.g. 16kHz <-> 24kHz <-> 48kHz)
                if !h_out.is_null() && current_rate != pkt_rate {
                    waveOutReset(h_out);
                    for i in 0..OUT_RING_COUNT {
                        let h_ptr = headers_ptr.add(i);
                        if (ptr::read_volatile(&(*h_ptr).dw_flags) & WHDR_PREPARED) != 0 {
                            waveOutUnprepareHeader(h_out, h_ptr, hdr_size);
                        }
                        (*h_ptr).dw_flags = 0;
                    }
                    waveOutClose(h_out);
                    h_out = ptr::null_mut();
                }

                if h_out.is_null() {
                    let wfx = WAVEFORMATEX {
                        w_format_tag: WAVE_FORMAT_PCM,
                        n_channels: CHANNELS,
                        n_samples_per_sec: pkt_rate,
                        n_avg_bytes_per_sec: pkt_rate * block_align as u32,
                        n_block_align: block_align,
                        w_bits_per_sample: BITS_PER_SAMPLE,
                        cb_size: 0,
                    };
                    if waveOutOpen(&mut h_out, WAVE_MAPPER, &wfx, 0, 0, CALLBACK_NULL) == 0 && !h_out.is_null() {
                        current_rate = pkt_rate;
                        let _ = waveOutSetVolume(h_out, 0xFFFFFFFF);
                        waveOutPause(h_out);
                        is_paused = true;
                        prebuf_count = 0;
                        next_slot = 0;
                        logger::info("desk_rt.audio_out", &format!("Speaker playback opened ({}Hz Mono).", pkt_rate));
                    } else {
                        h_out = ptr::null_mut();
                        continue;
                    }
                }

                let mut active_in_queue = 0usize;
                for i in 0..OUT_RING_COUNT {
                    let flags = ptr::read_volatile(&(*headers_ptr.add(i)).dw_flags);
                    if (flags & WHDR_INQUEUE) != 0 {
                        active_in_queue += 1;
                    }
                }

                if active_in_queue == 0 && !is_paused {
                    waveOutPause(h_out);
                    is_paused = true;
                    prebuf_count = 0;
                }

                let copy_len = pcm.len().min(MAX_OUT_BUF) & !1;
                if copy_len == 0 {
                    continue;
                }
                for _ in 0..OUT_RING_COUNT {
                    let h_ptr = headers_ptr.add(next_slot);
                    let flags = ptr::read_volatile(&(*h_ptr).dw_flags);
                    if (flags & WHDR_INQUEUE) == 0 {
                        if (flags & WHDR_PREPARED) != 0 {
                            waveOutUnprepareHeader(h_out, h_ptr, hdr_size);
                        }
                        let buf_ptr = buffers[next_slot].as_mut_ptr();
                        ptr::copy_nonoverlapping(pcm.as_ptr(), buf_ptr, copy_len);
                        (*h_ptr).lp_data = buf_ptr;
                        (*h_ptr).dw_buffer_length = copy_len as u32;
                        (*h_ptr).dw_flags = 0;
                        if waveOutPrepareHeader(h_out, h_ptr, hdr_size) == 0 {
                            waveOutWrite(h_out, h_ptr, hdr_size);
                            if is_paused {
                                prebuf_count += 1;
                                if prebuf_count >= 2 {
                                    waveOutRestart(h_out);
                                    is_paused = false;
                                }
                            }
                        }
                        next_slot = (next_slot + 1) % OUT_RING_COUNT;
                        break;
                    }
                    next_slot = (next_slot + 1) % OUT_RING_COUNT;
                }
            }

            if !h_out.is_null() {
                waveOutReset(h_out);
                for i in 0..OUT_RING_COUNT {
                    let h_ptr = headers_ptr.add(i);
                    if (ptr::read_volatile(&(*h_ptr).dw_flags) & WHDR_PREPARED) != 0 {
                        waveOutUnprepareHeader(h_out, h_ptr, hdr_size);
                    }
                }
                waveOutClose(h_out);
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
