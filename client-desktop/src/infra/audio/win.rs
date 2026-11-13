//! Session audio on Windows. Voice call through waveIn/waveOut: captures the microphone into binary voice
//! packets (`[0x02][rate_khz][pcm16le]`, or Opus) for the outbound queue and plays received packets, with a
//! light DSP stage (high-pass, noise gate, soft limiter) on captured audio. Host sound: WASAPI loopback of the
//! default output device into stereo Opus packets, and stereo waveOut playback on the viewer.

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient,
    IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX as MixFormat, eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};

use super::host_sound::{HostSoundReceiver, HostSoundSender, LoopbackConverter};
use super::{AudioMode, Route, SessionAudio, VoiceDspState, VoiceReceiver, VoiceSender, quality_to_audio_params};
use crate::infra::logger;
use crate::protocol::Outgoing;

/// This backend streams and plays host sound (`audio-system`).
pub const HOST_SOUND: bool = true;

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
/// Host sound plays in stereo.
const SOUND_CHANNELS: u16 = 2;
const BITS_PER_SAMPLE: u16 = 16;
/// Packets buffered before playback starts: 40 ms for voice, 60 ms for host sound, which tolerates more delay
/// than a conversation and plays continuously.
const VOICE_PREBUFFER: usize = 2;
const SOUND_PREBUFFER: usize = 3;
/// Loopback buffer requested from WASAPI (100 ns units): 200 ms, polled every 10 ms.
const LOOPBACK_BUFFER_HNS: i64 = 2_000_000;
const LOOPBACK_POLL: Duration = Duration::from_millis(10);
/// `WAVE_FORMAT_IEEE_FLOAT`, `WAVE_FORMAT_EXTENSIBLE`, and where `WAVEFORMATEXTENSIBLE` keeps the first field of
/// its sub-format GUID (the format tag the GUID stands for).
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;
const SUBFORMAT_OFFSET: usize = 24;
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

/// Audio of the current session: two-way voice (microphone capture into binary packets and playback) or host
/// sound, as [`VoiceEngine::mode`] says.
pub struct VoiceEngine {
    tx_play: Sender<(u32, Vec<u8>)>,
    rx: VoiceReceiver,
    tx_sound: Sender<(u32, Vec<u8>)>,
    sound_rx: HostSoundReceiver,
    pub is_session_active: Arc<AtomicBool>,
    pub is_mic_muted: Arc<AtomicBool>,
    pub audio_quality: Arc<AtomicUsize>,
    /// The voice partner listed `audio-opus`: send Opus instead of PCM.
    pub use_opus: Arc<AtomicBool>,
    /// Voice call or host sound. The microphone opens only in a voice call, the loopback only in
    /// `SendHostSound`, both only while `is_session_active` is set.
    pub mode: Arc<AudioMode>,
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
        let use_opus = Arc::new(AtomicBool::new(false));
        let mode = Arc::new(AudioMode::default());

        let active_cap = Arc::clone(&is_session_active);
        let muted_cap = Arc::clone(&is_mic_muted);
        let quality_cap = Arc::clone(&audio_quality);
        let opus_cap = Arc::clone(&use_opus);
        let mode_cap = Arc::clone(&mode);
        let in_call = move || active_cap.load(Ordering::Relaxed) && mode_cap.get() == SessionAudio::Voice;
        let tx_mic = tx_out.clone();

        // 1. Microphone Capture Thread
        // Keeps waveIn open across mute/unmute toggles during a session so re-enabling mic never fails!
        crate::infra::thread::spawn_named("audio-capture", move || unsafe {
            let block_align = (CHANNELS * BITS_PER_SAMPLE) / 8;
            let mut processed_buf = [0u8; MAX_FRAME_BYTES];
            // Failed opens in a row: a host without a microphone fails every time, so only the first is logged
            // and retries slow down to every 5 s.
            let mut open_failures = 0u32;

            loop {
                // Host sound pauses the call: the microphone closes (and the OS stops showing it in use).
                while !in_call() {
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
                    if open_failures == 0 {
                        logger::warn(
                            "desk_rt.audio_in",
                            &format!("waveInOpen ({}Hz) failed: {}; retrying quietly.", sample_rate, res),
                        );
                    }
                    open_failures += 1;
                    thread::sleep(Duration::from_millis(if open_failures < 4 { 500 } else { 5000 }));
                    continue;
                }
                open_failures = 0;

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
                let mut sender = VoiceSender::new(sample_rate, rate_tag_khz);
                let mut idx = 0usize;

                while in_call() && quality_cap.load(Ordering::Relaxed) == cur_quality {
                    let h_ptr = headers_ptr.add(idx);
                    // Volatile read is mandatory so LLVM --release never caches dw_flags!
                    let flags = ptr::read_volatile(&(*h_ptr).dw_flags);
                    if (flags & WHDR_DONE) != 0 {
                        let recorded = (ptr::read_volatile(&(*h_ptr).dw_bytes_recorded) as usize).min(frame_bytes) & !1;
                        if recorded > 0 && !muted_cap.load(Ordering::Relaxed) {
                            let slice = std::slice::from_raw_parts((*h_ptr).lp_data, recorded);
                            dsp.process_pcm_16le(slice, &mut processed_buf[..recorded]);
                            let pkt = sender.packet(&processed_buf[..recorded], opus_cap.load(Ordering::Relaxed));
                            let _ = tx_mic.send(Outgoing::Audio(pkt));
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

        // 2. Speaker playback: voice (16 / 24 / 48 kHz mono) and host sound (48 kHz stereo) each on their own waveOut.
        crate::infra::thread::spawn_named("audio-playback", move || unsafe {
            play_loop(rx_play, CHANNELS, VOICE_PREBUFFER)
        });
        let (tx_sound, rx_sound) = mpsc::channel::<(u32, Vec<u8>)>();
        crate::infra::thread::spawn_named("sound-playback", move || unsafe {
            play_loop(rx_sound, SOUND_CHANNELS, SOUND_PREBUFFER)
        });

        // 3. Host sound capture (loopback of the default output device) while hosting in `SendHostSound`.
        let (active_loop, mode_loop) = (Arc::clone(&is_session_active), Arc::clone(&mode));
        crate::infra::thread::spawn_named("sound-capture", move || {
            let streaming =
                move || active_loop.load(Ordering::Relaxed) && mode_loop.get() == SessionAudio::SendHostSound;
            let mut failures = 0u32;
            loop {
                while !streaming() {
                    failures = 0;
                    thread::sleep(Duration::from_millis(25));
                }
                match capture_loopback(&tx_out, &streaming) {
                    Ok(()) => failures = 0,
                    Err(e) => {
                        // A host without an output device fails every time: log once, retry slowly.
                        if failures == 0 {
                            logger::warn(
                                "desk_rt.sound_in",
                                &format!("Host sound capture failed: {e}; retrying quietly."),
                            );
                        }
                        failures += 1;
                        thread::sleep(Duration::from_millis(if failures < 4 { 500 } else { 5000 }));
                    }
                }
            }
        });

        Arc::new(Self {
            tx_play,
            rx: VoiceReceiver::new(),
            tx_sound,
            sound_rx: HostSoundReceiver::new(),
            is_session_active,
            is_mic_muted,
            audio_quality,
            use_opus,
            mode,
        })
    }

    /// Plays an incoming `0x02` packet as the current mode allows: voice (PCM `[0x02, rate_khz, pcm...]`, legacy
    /// `[0x02, pcm...]`, or Opus) in a voice call, host sound on a viewer that asked for it.
    pub fn play_binary_packet(&self, bin: &[u8]) {
        if !self.is_session_active.load(Ordering::Relaxed) || bin.len() <= 2 {
            return;
        }
        match super::route(bin, self.mode.get()) {
            Route::Voice => self.rx.receive(bin, |rate_hz, pcm| {
                let _ = self.tx_play.send((rate_hz, pcm));
            }),
            Route::HostSound => self.sound_rx.receive(bin, |pcm| {
                let _ = self.tx_sound.send((super::opus::DECODE_RATE, pcm));
            }),
            Route::Drop => {}
        }
    }
}

/// Captures the default output device by WASAPI loopback and queues host sound packets while `streaming()`
/// holds. Returns `Ok` when streaming ended, `Err` when the device could not be opened or failed (unplugged,
/// format change); the caller retries.
fn capture_loopback(tx_out: &Sender<Outgoing>, streaming: &dyn Fn() -> bool) -> windows::core::Result<()> {
    // SAFETY: COM is initialized for this thread for the duration of the capture and released on every path
    // below; the interfaces are dropped (released) before `CoUninitialize` because `run_loopback` owns them.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let result = unsafe { run_loopback(tx_out, streaming) };
    unsafe { CoUninitialize() };
    result
}

/// SAFETY: call with COM initialized on this thread. `GetMixFormat` returns a CoTaskMem block that is read with
/// unaligned reads (the struct is packed) and freed once; `GetBuffer` data is read only up to the frame count it
/// reports times the format's block size, and released before the next `GetBuffer`.
unsafe fn run_loopback(tx_out: &Sender<Outgoing>, streaming: &dyn Fn() -> bool) -> windows::core::Result<()> {
    let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
    let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? };
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let fmt_ptr = unsafe { client.GetMixFormat()? };
    let fmt: MixFormat = unsafe { ptr::read_unaligned(fmt_ptr) };
    let tag = fmt.wFormatTag;
    let float = tag == FORMAT_FLOAT
        || (tag == FORMAT_EXTENSIBLE
            && fmt.cbSize >= 22
            && unsafe { ptr::read_unaligned(fmt_ptr.cast::<u8>().add(SUBFORMAT_OFFSET).cast::<u32>()) }
                == u32::from(FORMAT_FLOAT));
    let (rate, channels, block_align) = (fmt.nSamplesPerSec, fmt.nChannels as usize, fmt.nBlockAlign as usize);
    let init = unsafe {
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, LOOPBACK_BUFFER_HNS, 0, fmt_ptr, None)
    };
    unsafe { CoTaskMemFree(Some(fmt_ptr as *const c_void)) };
    init?;
    let Some(mut convert) = LoopbackConverter::new(rate, channels, block_align, float) else {
        return Err(windows::core::Error::new(
            windows::Win32::Foundation::E_NOTIMPL,
            format!("unsupported mix format: tag {tag:#x}, {channels} ch, {block_align} B/frame"),
        ));
    };
    let capture: IAudioCaptureClient = unsafe { client.GetService()? };
    unsafe { client.Start()? };
    logger::info("desk_rt.sound_in", &format!("Host sound capture active ({rate} Hz, {channels} ch, float={float})."));

    let mut sender = HostSoundSender::new();
    let mut silence = Vec::new();
    let result = (|| {
        while streaming() {
            while unsafe { capture.GetNextPacketSize()? } > 0 {
                let (mut data, mut frames, mut flags) = (ptr::null_mut(), 0u32, 0u32);
                unsafe { capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)? };
                let bytes = frames as usize * convert.block_align();
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    silence.resize(bytes, 0);
                    convert.push(&silence, sender.buffer());
                } else {
                    convert.push(unsafe { std::slice::from_raw_parts(data, bytes) }, sender.buffer());
                }
                unsafe { capture.ReleaseBuffer(frames)? };
                sender.drain(|pkt| {
                    let _ = tx_out.send(Outgoing::Audio(pkt));
                });
            }
            thread::sleep(LOOPBACK_POLL);
        }
        Ok(())
    })();
    unsafe {
        let _ = client.Stop();
    }
    logger::info("desk_rt.sound_in", "Host sound capture stopped.");
    result
}

/// Plays `(rate_hz, pcm16le)` chunks on a waveOut device with `channels` channels, reopening it when the rate
/// changes, with a jitter prebuffer of `prebuffer` chunks. Runs until the sender is dropped.
///
/// SAFETY: the waveOut headers and buffers live on this function's stack frame (boxed) for as long as the device
/// is open; every header is unprepared before the device closes.
unsafe fn play_loop(rx_play: Receiver<(u32, Vec<u8>)>, channels: u16, prebuffer: usize) {
    unsafe {
        let block_align = (channels * BITS_PER_SAMPLE) / 8;
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
                    n_channels: channels,
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
                    logger::info(
                        "desk_rt.audio_out",
                        &format!("Speaker playback opened ({pkt_rate} Hz, {channels} ch)."),
                    );
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
                            if prebuf_count >= prebuffer {
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
    }
}
