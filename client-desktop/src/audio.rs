use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use core_engine::codec::to_base64;

use crate::logger;
use crate::protocol::OutgoingMsg;

type HWAVEIN = *mut c_void;
type HWAVEOUT = *mut c_void;
type MMRESULT = u32;

const WAVE_MAPPER: u32 = 0xFFFFFFFF;
const WAVE_FORMAT_PCM: u16 = 1;
const CALLBACK_NULL: u32 = 0x00000000;
const WHDR_DONE: u32 = 0x00000001;
const WHDR_PREPARED: u32 = 0x00000002;
const WHDR_INQUEUE: u32 = 0x00000010;

// 16 kHz, 16-bit Mono PCM = 32,000 bytes/sec (1,280 bytes per 40ms frame)
const SAMPLE_RATE: u32 = 16000;
const CHANNELS: u16 = 1;
const BITS_PER_SAMPLE: u16 = 16;
const FRAME_BYTES: usize = 1280; // 40ms
const IN_RING_COUNT: usize = 8;
const OUT_RING_COUNT: usize = 16;
const MAX_OUT_BUF: usize = 4096;

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

/// Applies 2.5x software microphone gain with soft-saturation clamping on 16-bit signed PCM
fn boost_pcm_16le(raw: &[u8], out: &mut [u8]) {
    let n = raw.len().min(out.len()) & !1;
    let mut i = 0;
    while i < n {
        let sample = i16::from_le_bytes([raw[i], raw[i + 1]]) as i32;
        let boosted = ((sample * 5) / 2).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let bytes = boosted.to_le_bytes();
        out[i] = bytes[0];
        out[i + 1] = bytes[1];
        i += 2;
    }
}

pub struct VoiceEngine {
    tx_play: Sender<Vec<u8>>,
    pub is_session_active: Arc<AtomicBool>,
    pub is_mic_muted: Arc<AtomicBool>,
}

impl VoiceEngine {
    pub fn new(tx_out: Sender<OutgoingMsg>) -> Arc<Self> {
        unsafe {
            // Enable 1.0ms OS scheduler timer resolution for ultra-low network & audio latency
            timeBeginPeriod(1);
        }

        let (tx_play, rx_play) = mpsc::channel::<Vec<u8>>();
        let is_session_active = Arc::new(AtomicBool::new(false));
        // Default Mic ON (unmuted = false) when a remote session starts!
        let is_mic_muted = Arc::new(AtomicBool::new(false));

        let active_cap = Arc::clone(&is_session_active);
        let muted_cap = Arc::clone(&is_mic_muted);

        // 1. Microphone Capture Thread (waveIn 16kHz Mono PCM + 2.5x Pre-Amp Gain)
        thread::spawn(move || unsafe {
            let block_align = (CHANNELS * BITS_PER_SAMPLE) / 8;
            let wfx = WAVEFORMATEX {
                w_format_tag: WAVE_FORMAT_PCM,
                n_channels: CHANNELS,
                n_samples_per_sec: SAMPLE_RATE,
                n_avg_bytes_per_sec: SAMPLE_RATE * block_align as u32,
                n_block_align: block_align,
                w_bits_per_sample: BITS_PER_SAMPLE,
                cb_size: 0,
            };

            let mut boosted_buf = [0u8; FRAME_BYTES];

            loop {
                while !active_cap.load(Ordering::Relaxed) || muted_cap.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(25));
                }

                let mut h_in: HWAVEIN = ptr::null_mut();
                let res = waveInOpen(&mut h_in, WAVE_MAPPER, &wfx, 0, 0, CALLBACK_NULL);
                if res != 0 || h_in.is_null() {
                    logger::warn("desk_rt.audio_in", &format!("waveInOpen failed: {}", res));
                    thread::sleep(Duration::from_millis(800));
                    continue;
                }

                let mut buffers: Vec<Box<[u8; FRAME_BYTES]>> = (0..IN_RING_COUNT)
                    .map(|_| Box::new([0u8; FRAME_BYTES]))
                    .collect();

                let mut headers: Box<[WAVEHDR; IN_RING_COUNT]> = Box::new([WAVEHDR {
                    lp_data: ptr::null_mut(),
                    dw_buffer_length: FRAME_BYTES as u32,
                    dw_bytes_recorded: 0,
                    dw_user: 0,
                    dw_flags: 0,
                    dw_loops: 0,
                    lp_next: ptr::null_mut(),
                    reserved: 0,
                }; IN_RING_COUNT]);
                let hdr_size = std::mem::size_of::<WAVEHDR>() as u32;
                let headers_ptr = headers.as_mut_ptr();

                for i in 0..IN_RING_COUNT {
                    let h_ptr = headers_ptr.add(i);
                    (*h_ptr).lp_data = buffers[i].as_mut_ptr();
                    (*h_ptr).dw_buffer_length = FRAME_BYTES as u32;
                    (*h_ptr).dw_bytes_recorded = 0;
                    (*h_ptr).dw_flags = 0;
                    waveInPrepareHeader(h_in, h_ptr, hdr_size);
                    waveInAddBuffer(h_in, h_ptr, hdr_size);
                }

                waveInStart(h_in);
                logger::info("desk_rt.audio_in", "Microphone capture active (16kHz Mono).");

                let mut idx = 0usize;
                while active_cap.load(Ordering::Relaxed) && !muted_cap.load(Ordering::Relaxed) {
                    let h_ptr = headers_ptr.add(idx);
                    // Volatile read is mandatory so LLVM --release never caches dw_flags!
                    let flags = ptr::read_volatile(&(*h_ptr).dw_flags);
                    if (flags & WHDR_DONE) != 0 {
                        let recorded = (ptr::read_volatile(&(*h_ptr).dw_bytes_recorded) as usize)
                            .min(FRAME_BYTES);
                        if recorded > 0 {
                            let slice = std::slice::from_raw_parts((*h_ptr).lp_data, recorded);
                            boost_pcm_16le(slice, &mut boosted_buf[..recorded]);
                            let b64 = to_base64(&boosted_buf[..recorded]);
                            let _ = tx_out.send(OutgoingMsg::AudioChunk(b64));
                        }
                        waveInUnprepareHeader(h_in, h_ptr, hdr_size);
                        (*h_ptr).dw_bytes_recorded = 0;
                        (*h_ptr).dw_flags = 0;
                        (*h_ptr).dw_buffer_length = FRAME_BYTES as u32;
                        waveInPrepareHeader(h_in, h_ptr, hdr_size);
                        waveInAddBuffer(h_in, h_ptr, hdr_size);
                        idx = (idx + 1) % IN_RING_COUNT;
                    } else {
                        thread::sleep(Duration::from_millis(4));
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
                logger::info("desk_rt.audio_in", "Microphone capture paused.");
            }
        });

        // 2. Speaker Playback Thread (waveOut 16kHz Mono PCM with Jitter Pre-Buffering)
        thread::spawn(move || unsafe {
            let block_align = (CHANNELS * BITS_PER_SAMPLE) / 8;
            let wfx = WAVEFORMATEX {
                w_format_tag: WAVE_FORMAT_PCM,
                n_channels: CHANNELS,
                n_samples_per_sec: SAMPLE_RATE,
                n_avg_bytes_per_sec: SAMPLE_RATE * block_align as u32,
                n_block_align: block_align,
                w_bits_per_sample: BITS_PER_SAMPLE,
                cb_size: 0,
            };

            let mut h_out: HWAVEOUT = ptr::null_mut();
            let mut buffers: Vec<Box<[u8; MAX_OUT_BUF]>> = (0..OUT_RING_COUNT)
                .map(|_| Box::new([0u8; MAX_OUT_BUF]))
                .collect();

            let mut headers: Box<[WAVEHDR; OUT_RING_COUNT]> = Box::new([WAVEHDR {
                lp_data: ptr::null_mut(),
                dw_buffer_length: 0,
                dw_bytes_recorded: 0,
                dw_user: 0,
                dw_flags: 0,
                dw_loops: 0,
                lp_next: ptr::null_mut(),
                reserved: 0,
            }; OUT_RING_COUNT]);
            let hdr_size = std::mem::size_of::<WAVEHDR>() as u32;
            let headers_ptr = headers.as_mut_ptr();

            let mut next_slot = 0usize;
            let mut is_paused = false;
            let mut prebuf_count = 0usize;

            loop {
                let pcm = match rx_play.recv_timeout(Duration::from_millis(60)) {
                    Ok(data) => data,
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

                // Lazily open waveOut if not yet open
                if h_out.is_null() {
                    if waveOutOpen(&mut h_out, WAVE_MAPPER, &wfx, 0, 0, CALLBACK_NULL) == 0
                        && !h_out.is_null()
                    {
                        let _ = waveOutSetVolume(h_out, 0xFFFFFFFF); // 100% Left + Right volume
                        waveOutPause(h_out);
                        is_paused = true;
                        prebuf_count = 0;
                        logger::info(
                            "desk_rt.audio_out",
                            "Speaker playback device opened (16kHz Mono).",
                        );
                    } else {
                        h_out = ptr::null_mut();
                        continue;
                    }
                }

                // Check how many buffers are currently in hardware queue
                let mut active_in_queue = 0usize;
                for i in 0..OUT_RING_COUNT {
                    let flags = ptr::read_volatile(&(*headers_ptr.add(i)).dw_flags);
                    if (flags & WHDR_INQUEUE) != 0 {
                        active_in_queue += 1;
                    }
                }

                // If hardware queue drained completely, pause briefly to pre-buffer 2 frames (80ms)
                // so Windows Audio Engine never underruns on network jitter
                if active_in_queue == 0 && !is_paused {
                    waveOutPause(h_out);
                    is_paused = true;
                    prebuf_count = 0;
                }

                let copy_len = pcm.len().min(MAX_OUT_BUF);
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

        Arc::new(Self {
            tx_play,
            is_session_active,
            is_mic_muted,
        })
    }

    pub fn play_pcm_chunk(&self, pcm: &[u8]) {
        if self.is_session_active.load(Ordering::Relaxed) && !pcm.is_empty() {
            let _ = self.tx_play.send(pcm.to_vec());
        }
    }
}
