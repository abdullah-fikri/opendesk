use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_engine::codec::{from_base64, to_base64};
use core_engine::input::InputInjector;
use serde_json::{json, Value};
use slint::Weak;

use crate::logger;
use crate::protocol::OutgoingMsg;
use crate::MainWindow;

const CHUNK_SIZE: usize = 32768; // 32 KB per chunk for smooth non-blocking WebSocket flow

pub struct FileReceiver {
    active_files: HashMap<String, (File, PathBuf, String, u64, u64)>,
}

impl FileReceiver {
    pub fn new() -> Self {
        Self {
            active_files: HashMap::new(),
        }
    }

    fn get_staging_dir() -> PathBuf {
        if let Ok(profile) = env::var("USERPROFILE") {
            let dl = PathBuf::from(profile).join("Downloads").join("OpenDesk");
            if fs::create_dir_all(&dl).is_ok() {
                return dl;
            }
        }
        let fallback = crate::config::get_app_dir().join("Downloads");
        let _ = fs::create_dir_all(&fallback);
        fallback
    }

    pub fn handle_payload(
        &mut self,
        payload: &Value,
        injector: &InputInjector,
        last_clip_seq: &Arc<AtomicU32>,
    ) -> Option<String> {
        let action = payload.get("action").and_then(|v| v.as_str())?;
        let id = payload.get("id").and_then(|v| v.as_str())?;

        match action {
            "start" => {
                let raw_name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("opendesk_file.bin");
                let total_size = payload.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                let clean_name: String = raw_name
                    .chars()
                    .map(|c| match c {
                        '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                        _ => c,
                    })
                    .collect();

                let dir = Self::get_staging_dir();
                let target_path = dir.join(&clean_name);
                if let Ok(file) = File::create(&target_path) {
                    logger::info(
                        "app.tunnel_ft_comp",
                        &format!(
                            "Receiving file transfer id={} name={} size={}",
                            id, clean_name, total_size
                        ),
                    );
                    self.active_files.insert(
                        id.to_string(),
                        (file, target_path, clean_name.clone(), 0, total_size),
                    );
                    return Some(format!("Menerima {}...", clean_name));
                }
            }
            "chunk" => {
                let b64 = payload.get("chunk").and_then(|v| v.as_str())?;
                if let Some(bytes) = from_base64(b64) {
                    if let Some((file, _, clean_name, received, total)) =
                        self.active_files.get_mut(id)
                    {
                        let _ = file.write_all(&bytes);
                        *received += bytes.len() as u64;
                        if *total > 0 {
                            let pct = ((*received * 100) / *total).min(99);
                            return Some(format!("Menerima {} ({}%)...", clean_name, pct));
                        }
                    }
                }
            }
            "end" => {
                let auto_paste = payload
                    .get("autoPaste")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if let Some((mut file, path, clean_name, _, _)) = self.active_files.remove(id) {
                    let _ = file.flush();
                    drop(file);

                    let abs_path = path.to_string_lossy().to_string();
                    injector.set_clipboard_files(&[abs_path.clone()]);
                    // Record new clipboard sequence number so local watcher doesn't echo the file back
                    last_clip_seq.store(injector.get_clipboard_sequence_number(), Ordering::SeqCst);

                    if auto_paste {
                        // Small delay so Explorer processes clipboard change before Ctrl+V
                        thread::sleep(Duration::from_millis(35));
                        injector.send_key_combo(true, false, false, 0x56); // Ctrl+V into open Explorer folder
                    }

                    logger::info(
                        "app.tunnel_ft_comp",
                        &format!(
                            "File transfer complete: {} (CF_HDROP set, auto_paste={})",
                            abs_path, auto_paste
                        ),
                    );
                    return Some(format!("Selesai: {} (Siap Paste / Downloads\\OpenDesk)", clean_name));
                }
            }
            _ => {}
        }
        None
    }
}

pub fn send_files_async(
    paths: Vec<PathBuf>,
    target_clean: String,
    tx_out: Sender<OutgoingMsg>,
    win_opt: Option<Weak<MainWindow>>,
    auto_paste: bool,
) {
    if target_clean.is_empty() || paths.is_empty() {
        return;
    }

    thread::spawn(move || {
        for path in paths {
            if !path.is_file() {
                continue;
            }

            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file.bin")
                .to_string();

            let mut file = match File::open(&path) {
                Ok(f) => f,
                Err(e) => {
                    logger::error(
                        "app.tunnel_ft_comp",
                        &format!("Failed to open file {:?}: {}", path, e),
                    );
                    continue;
                }
            };

            let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros();
            let transfer_id = format!("ft_{}", ts);

            logger::info(
                "app.tunnel_ft_comp",
                &format!(
                    "Starting file transfer id={} name={} bytes={} auto_paste={}",
                    transfer_id, file_name, file_size, auto_paste
                ),
            );

            let start_msg = json!({
                "type": "relay-file",
                "targetClean": target_clean,
                "payload": {
                    "action": "start",
                    "id": transfer_id,
                    "name": file_name,
                    "size": file_size
                }
            });
            let _ = tx_out.send(OutgoingMsg::Json(start_msg));

            let mut buf = vec![0u8; CHUNK_SIZE];
            let mut sent_bytes = 0u64;
            let mut chunk_idx = 0usize;

            loop {
                match file.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        sent_bytes += n as u64;
                        chunk_idx += 1;
                        let b64 = to_base64(&buf[..n]);
                        let chunk_msg = json!({
                            "type": "relay-file",
                            "targetClean": target_clean,
                            "payload": {
                                "action": "chunk",
                                "id": transfer_id,
                                "chunk": b64
                            }
                        });
                        let _ = tx_out.send(OutgoingMsg::Json(chunk_msg));

                        if file_size > 0 && (chunk_idx % 4 == 0 || sent_bytes == file_size) {
                            let pct = ((sent_bytes * 100) / file_size).min(99);
                            if let Some(ref w_weak) = win_opt {
                                let name_c = file_name.clone();
                                let _ = w_weak.upgrade_in_event_loop(move |win| {
                                    win.set_status_text(
                                        format!("Mengirim {} ({}%)...", name_c, pct).into(),
                                    );
                                });
                            }
                        }

                        // Pacing so chunks don't overwhelm Cloudflare tunnel or drop on non-blocking socket
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }

            let end_msg = json!({
                "type": "relay-file",
                "targetClean": target_clean,
                "payload": {
                    "action": "end",
                    "id": transfer_id,
                    "name": file_name,
                    "autoPaste": auto_paste
                }
            });
            let _ = tx_out.send(OutgoingMsg::Json(end_msg));

            if let Some(ref w_weak) = win_opt {
                let name_done = file_name.clone();
                let _ = w_weak.upgrade_in_event_loop(move |win| {
                    win.set_status_text(format!("Terkirim: {}", name_done).into());
                });
            }

            logger::info(
                "app.tunnel_ft_comp",
                &format!("Completed file transfer id={} name={}", transfer_id, file_name),
            );
        }
    });
}
