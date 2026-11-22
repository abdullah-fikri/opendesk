use super::*;
use serde_json::{Value, json};
use std::thread;

fn payload(v: Value) -> FilePayload {
    serde_json::from_value(v).unwrap()
}

fn entries(v: Value) -> Vec<OfferEntry> {
    serde_json::from_value(v).unwrap()
}

#[test]
fn descriptor_layout_matches_windows() {
    assert_eq!(std::mem::size_of::<FILEDESCRIPTORW>(), 592);
    assert_eq!(std::mem::size_of::<STATSTG>(), 80);
    let bytes = descriptor_bytes(&[OfferFile {
        name: "a:b.txt".into(),
        size: 5_000_000_000,
        dir: false,
        mtime: None,
        ctime: None,
        atime: None,
        attrs: 0,
        remote: 0,
    }]);
    assert_eq!(bytes.len(), 4 + 592);
    assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 1);
    let fd = &bytes[4..];
    assert_eq!(u32::from_le_bytes(fd[0..4].try_into().unwrap()), FD_FILESIZE | FD_PROGRESSUI);
    assert_eq!(u32::from_le_bytes(fd[64..68].try_into().unwrap()), 1); // size high
    let name: Vec<u16> = fd[72..86].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    assert_eq!(String::from_utf16_lossy(&name), "a_b.txt");
}

#[test]
fn descriptor_carries_write_time() {
    // 2024-01-01T00:00:00Z = 1_704_067_200_000 ms = FILETIME 133_485_408_000_000_000.
    let file = OfferFile {
        name: "a.txt".into(),
        size: 1,
        dir: false,
        mtime: Some(1_704_067_200_000),
        ctime: None,
        atime: None,
        attrs: 0,
        remote: 0,
    };
    let fd = &descriptor_bytes(&[file])[4..];
    assert_eq!(u32::from_le_bytes(fd[0..4].try_into().unwrap()), FD_FILESIZE | FD_PROGRESSUI | FD_WRITETIME);
    let low = u32::from_le_bytes(fd[56..60].try_into().unwrap()) as u64;
    let high = u32::from_le_bytes(fd[60..64].try_into().unwrap()) as u64;
    assert_eq!(high << 32 | low, 133_485_408_000_000_000);
    assert_eq!(unix_ms_to_filetime(u64::MAX), None);
}

/// Puts `offer` on the clipboard, pastes it with Explorer into a fresh temp folder and plays the partner,
/// answering each pull with `contents[remote index]`. Returns the folder, or None in manual mode.
fn paste_with_explorer(tag: &str, offer: Value, contents: &HashMap<u64, Vec<u8>>) -> Option<std::path::PathBuf> {
    // Both ignored tests use the one system clipboard.
    static CLIPBOARD: Mutex<()> = Mutex::new(());
    let _guard = CLIPBOARD.lock();
    let dir = std::env::temp_dir().join(format!("opendesk-paste-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (tx_out, rx_out) = mpsc::channel();
    assert!(set_offer("111", &payload(offer), tx_out).is_some());
    thread::sleep(Duration::from_millis(300));

    // A fresh process must hold the OLE clipboard object (as Explorer always does for its Paste menu state)
    // before pasting; otherwise Shell reads through Win32 clipboard emulation, which has no per-file lindex.
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; $null=[System.Windows.Forms.Clipboard]::GetDataObject().GetFormats(); $f=(New-Object -ComObject Shell.Application).NameSpace('{}'); $f.Self.InvokeVerb('Paste'); Start-Sleep -Seconds 10",
        dir.display()
    );
    // OPENDESK_MANUAL_PASTE=1: paste by hand (Ctrl+V in any Explorer folder) instead of the scripted paste.
    let manual = std::env::var("OPENDESK_MANUAL_PASTE").is_ok();
    let mut ps = if manual {
        println!("Clipboard now holds the virtual {}: press Ctrl+V in an Explorer folder within 60 s.", tag);
        None
    } else {
        Some(
            std::process::Command::new("powershell").args(["-NoProfile", "-STA", "-Command", &script]).spawn().unwrap(),
        )
    };

    // Play the partner: answer every pull with start/chunk/end.
    for _ in 0..contents.len() {
        let Outgoing::Message(ClientMessage::RelayFile { payload: pull, .. }) =
            rx_out.recv_timeout(Duration::from_secs(if manual { 60 } else { 40 })).expect("Explorer never pulled")
        else {
            panic!("unexpected message")
        };
        let tid = pull.tid.unwrap();
        let index = pull.index.unwrap();
        let data = contents.get(&index).expect("pull for a folder or unknown entry");
        on_transfer("111", FileAction::Start, &tid, &payload(json!({ "size": data.len() })));
        for chunk in data.chunks(32768) {
            on_transfer(
                "111",
                FileAction::Chunk,
                &tid,
                &payload(json!({ "chunk": core_engine::codec::to_base64(chunk) })),
            );
        }
        let sha: String = Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            on_transfer("111", FileAction::End, &tid, &payload(json!({ "sha256": sha }))).unwrap().1,
            Some(true)
        );
    }

    let result = if let Some(ps) = ps.as_mut() {
        let _ = ps.wait();
        Some(dir)
    } else {
        thread::sleep(Duration::from_secs(3)); // let Explorer finish writing; check the pasted files by hand
        None
    };
    clear_offer("");
    result
}

/// A window of another process accepts a drag started with no mouse button held, at the pointer position.
/// Moves the real pointer, so run it by hand: `cargo test file_drop_reaches_a_window -- --ignored --nocapture`
#[test]
#[ignore]
fn file_drop_reaches_a_window_under_the_pointer() {
    let dir = std::env::temp_dir().join(format!("opendesk-drop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ready, got) = (dir.join("ready"), dir.join("formats"));
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; $f=New-Object System.Windows.Forms.Form; \
         $f.StartPosition='Manual'; $f.Location=New-Object System.Drawing.Point(300,300); $f.Size=New-Object System.Drawing.Size(400,300); \
         $f.TopMost=$true; $f.AllowDrop=$true; \
         $f.Add_DragEnter({{ $_.Effect='Copy' }}); \
         $f.Add_DragDrop({{ [IO.File]::WriteAllText('{got}', ($_.Data.GetFormats() -join ',')); $f.Close() }}); \
         $f.Add_Shown({{ [IO.File]::WriteAllText('{ready}', '1') }}); \
         $t=New-Object System.Windows.Forms.Timer; $t.Interval=20000; $t.Add_Tick({{ $f.Close() }}); $t.Start(); \
         [void]$f.ShowDialog()",
        got = got.display(),
        ready = ready.display()
    );
    let mut ps =
        std::process::Command::new("powershell").args(["-NoProfile", "-STA", "-Command", &script]).spawn().unwrap();
    for _ in 0..100 {
        if ready.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    assert!(ready.exists(), "the target window never opened");
    let (tx_out, _rx) = mpsc::channel();
    let offer = json!({ "id": "of_d", "files": [{ "name": "dropped.bin", "size": 10 }] });
    // The host's dispatcher puts the pointer on the spot first.
    unsafe { SetCursorPos(500, 450) };
    thread::sleep(Duration::from_millis(200));
    assert!(drop_offer("111", &payload(offer), tx_out).is_some());
    for _ in 0..80 {
        if got.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = ps.kill();
    let _ = ps.wait();
    let formats = std::fs::read_to_string(&got).expect("the window never received the drop");
    assert!(formats.contains("FileGroupDescriptorW"), "formats: {formats}");
    clear_offer("");
    let _ = std::fs::remove_dir_all(dir);
}

/// Real Explorer paste across processes. Overwrites the clipboard, so run it by hand:
/// `cargo test --release explorer_paste -- --ignored --nocapture`
#[test]
#[ignore]
fn explorer_paste_pulls_virtual_file() {
    let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let offer = json!({ "id": "of_t", "files": [{ "name": "pasted.bin", "size": data.len() }] });
    if let Some(dir) = paste_with_explorer("file", offer, &HashMap::from([(0, data.clone())])) {
        let pasted = std::fs::read(dir.join("pasted.bin")).expect("Explorer did not create the file");
        assert_eq!(pasted, data);
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// The service setup: a separate helper process holds the clipboard object and gets the bytes from this
/// process (the agent) over its pipes. Needs the app built first:
/// `cargo build --release && cargo test --release explorer_paste_through_helper -- --ignored --nocapture`
#[test]
#[ignore]
fn explorer_paste_through_helper_process() {
    let deps = std::env::current_exe().unwrap();
    let exe = deps.parent().unwrap().parent().unwrap().join("opendesk-client.exe");
    let mut helper = crate::infra::clipboard_helper::start_test_helper(&exe);
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 13 % 251) as u8).collect();
    let small = b"second file".to_vec();
    let offer = json!({ "id": "of_h", "files": [
        { "name": "big.bin", "size": data.len(), "mtime": 1_704_067_200_000u64 },
        { "name": "small.txt", "size": small.len() }
    ]});
    let contents = HashMap::from([(0, data.clone()), (1, small.clone())]);
    let dir = paste_with_explorer("helper", offer, &contents);
    let _ = helper.kill();
    let _ = helper.wait();
    if let Some(dir) = dir {
        assert_eq!(std::fs::read(dir.join("big.bin")).expect("big.bin not pasted"), data);
        assert_eq!(std::fs::read(dir.join("small.txt")).expect("small.txt not pasted"), small);
        let modified = std::fs::metadata(dir.join("big.bin")).unwrap().modified().unwrap();
        let ms = modified.duration_since(UNIX_EPOCH).unwrap().as_millis();
        assert_eq!(ms, 1_704_067_200_000, "Date Modified kept (FN1)");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Same, for several top-level files and no folder (a multi-file copy in Explorer): every file must be
/// pulled and pasted, not only the first.
#[test]
#[ignore]
fn explorer_paste_pulls_four_top_level_files() {
    let names = ["a.txt", "b.bin", "c.dat", "d.log"];
    let contents: HashMap<u64, Vec<u8>> = (0..names.len() as u64)
        .map(|i| (i, (0..40_000 + i as u32 * 30_000).map(|b| (b * (i as u32 + 3) % 251) as u8).collect()))
        .collect();
    let files: Vec<Value> =
        names.iter().enumerate().map(|(i, n)| json!({ "name": n, "size": contents[&(i as u64)].len() })).collect();
    let offer = json!({ "id": "of_four", "files": files });
    if let Some(dir) = paste_with_explorer("four", offer, &contents) {
        for (i, name) in names.iter().enumerate() {
            let pasted = std::fs::read(dir.join(name)).unwrap_or_else(|_| panic!("{name} was not pasted"));
            assert_eq!(pasted, contents[&(i as u64)], "{name}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Same, for a nested folder with an empty subfolder; an unsafe entry in the offer must be dropped and the
/// remaining pulls must still name the partner's own indices.
#[test]
#[ignore]
fn explorer_paste_recreates_nested_folder() {
    let main_rs = b"fn main() {}\n".to_vec();
    let big: Vec<u8> = (0..150_000u32).map(|i| (i * 13 % 251) as u8).collect();
    let offer = json!({ "id": "of_tree", "files": [
        { "name": "Proyek", "dir": true },
        { "name": "..\\escape.txt", "size": 3 },
        { "name": "Proyek\\src", "dir": true },
        { "name": "Proyek\\src\\main.rs", "size": main_rs.len() },
        { "name": "Proyek\\kosong", "dir": true },
        { "name": "Proyek\\src\\data.bin", "size": big.len() },
    ]});
    let contents = HashMap::from([(3, main_rs.clone()), (5, big.clone())]);
    if let Some(dir) = paste_with_explorer("tree", offer, &contents) {
        assert_eq!(std::fs::read(dir.join(r"Proyek\src\main.rs")).expect("main.rs missing"), main_rs);
        assert_eq!(std::fs::read(dir.join(r"Proyek\src\data.bin")).expect("data.bin missing"), big);
        assert!(dir.join(r"Proyek\kosong").is_dir(), "empty folder not recreated");
        assert!(!dir.join("escape.txt").exists() && !dir.parent().unwrap().join("escape.txt").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn descriptor_marks_nested_folders_and_relative_names() {
    let files = parse_offer_files(&entries(json!([
        { "name": "Proyek", "dir": true },
        { "name": "Proyek/src", "dir": true },
        { "name": "Proyek\\src\\main.rs", "size": 7 },
    ])))
    .unwrap();
    let bytes = descriptor_bytes(&files);
    assert_eq!(bytes.len(), 4 + 3 * 592);
    assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 3);
    let fd = |i: usize| &bytes[4 + i * 592..4 + (i + 1) * 592];
    let name = |i: usize| {
        let raw: Vec<u16> = fd(i)[72..72 + 520].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&raw[..raw.iter().position(|&c| c == 0).unwrap()])
    };
    let u32_at = |i: usize, off: usize| u32::from_le_bytes(fd(i)[off..off + 4].try_into().unwrap());
    for i in 0..2 {
        assert_eq!(u32_at(i, 0), FD_ATTRIBUTES | FD_PROGRESSUI);
        assert_eq!(u32_at(i, 36), FILE_ATTRIBUTE_DIRECTORY); // dwFileAttributes
    }
    assert_eq!(u32_at(2, 0), FD_FILESIZE | FD_PROGRESSUI);
    assert_eq!(u32_at(2, 68), 7); // size low
    assert_eq!((name(0), name(1), name(2)), ("Proyek".into(), r"Proyek\src".into(), r"Proyek\src\main.rs".into()));
}

#[test]
fn descriptor_carries_all_three_times_and_safe_attributes_only() {
    // System (0x4) must not cross; read-only, hidden, archive do.
    let files = parse_offer_files(&entries(json!([
        { "name": "a.txt", "size": 1, "mtime": 1_704_067_200_000u64, "ctime": 1_600_000_000_000u64,
          "atime": 1_710_000_000_000u64, "attrs": 0x1 | 0x2 | 0x4 | 0x20 },
    ])))
    .unwrap();
    let fd = &descriptor_bytes(&files)[4..];
    let u32_at = |off: usize| u32::from_le_bytes(fd[off..off + 4].try_into().unwrap());
    let ft_at = |off: usize| u32_at(off) as u64 | (u32_at(off + 4) as u64) << 32;
    assert_eq!(u32_at(0), FD_ATTRIBUTES | FD_CREATETIME | FD_ACCESSTIME | FD_WRITETIME | FD_FILESIZE | FD_PROGRESSUI);
    assert_eq!(u32_at(36), 0x1 | 0x2 | 0x20);
    assert_eq!(ft_at(40), unix_ms_to_filetime(1_600_000_000_000).unwrap()); // ftCreationTime
    assert_eq!(ft_at(48), unix_ms_to_filetime(1_710_000_000_000).unwrap()); // ftLastAccessTime
    assert_eq!(ft_at(56), unix_ms_to_filetime(1_704_067_200_000).unwrap()); // ftLastWriteTime
}

#[test]
fn offer_parsing_drops_unsafe_names_and_keeps_partner_indices() {
    let files = parse_offer_files(&entries(json!([
        { "name": "..\\x", "size": 1 },
        { "name": "C:\\Windows\\x", "size": 1 },
        { "name": "\\\\server\\share\\x", "size": 1 },
        { "name": "a\\NUL.txt", "size": 1 },
        { "name": "ok.txt", "size": 1 },
        { "name": "big.bin", "size": 2u64 << 40 },
        { "name": "d", "dir": true },
    ])))
    .unwrap();
    let kept: Vec<(&str, usize, bool)> = files.iter().map(|f| (f.name.as_str(), f.remote, f.dir)).collect();
    assert_eq!(kept, vec![("ok.txt", 4, false), ("d", 6, true)]);
    let too_many: Vec<OfferEntry> =
        (0..10_001).map(|i| OfferEntry { name: format!("f{i}"), size: Some(0), ..Default::default() }).collect();
    assert!(parse_offer_files(&too_many).is_err());
}

#[test]
fn folder_entries_are_never_pulled() {
    let (tx_out, rx_out) = mpsc::channel();
    let offer = Offer {
        from: "111".into(),
        offer_id: "of_d".into(),
        files: vec![OfferFile {
            name: "d".into(),
            size: 0,
            dir: true,
            mtime: None,
            ctime: None,
            atime: None,
            attrs: 0,
            remote: 0,
        }],
        route: Route::Local(tx_out),
        prefetch: Default::default(),
    };
    let obj = Box::into_raw(Box::new(DataObject { vtbl: &DATA_VTBL, refs: AtomicU32::new(1), offer: Arc::new(offer) }))
        as *mut c_void;
    let fmt = FORMATETC {
        cf_format: formats().contents,
        ptd: ptr::null_mut(),
        dw_aspect: DVASPECT_CONTENT,
        lindex: 0,
        tymed: TYMED_ISTREAM,
    };
    let mut medium = STGMEDIUM { tymed: 0, data: ptr::null_mut(), p_unk_for_release: ptr::null_mut() };
    unsafe {
        assert_eq!(data_get_data(obj, &fmt, &mut medium), DV_E_LINDEX);
        data_release(obj);
    }
    assert!(rx_out.try_recv().is_err(), "no pull may be sent for a folder");
}

#[test]
fn pulled_bytes_flow_to_the_stream_and_are_verified() {
    let (tx_out, _rx_out) = mpsc::channel();
    let offer = Offer {
        from: "111".into(),
        offer_id: "of_1".into(),
        files: vec![OfferFile {
            name: "x.bin".into(),
            size: 4,
            dir: false,
            mtime: None,
            ctime: None,
            atime: None,
            attrs: 0,
            remote: 0,
        }],
        route: Route::Local(tx_out),
        prefetch: Default::default(),
    };
    let stream = open_pull(&offer, 0);
    let tid = unsafe { (*(stream as *const PullStream)).tid.clone() };
    assert!(
        on_transfer("999", FileAction::Chunk, &tid, &payload(json!({}))).is_none(),
        "other peer must not feed the stream"
    );
    on_transfer("111", FileAction::Start, &tid, &payload(json!({ "size": 4 })));
    on_transfer("111", FileAction::Chunk, &tid, &payload(json!({ "chunk": core_engine::codec::to_base64(b"abcd") })));
    let sha: String = Sha256::digest(b"abcd").iter().map(|b| format!("{b:02x}")).collect();
    let (_, ack) = on_transfer("111", FileAction::End, &tid, &payload(json!({ "sha256": sha }))).unwrap();
    assert_eq!(ack, Some(true));
    let mut buf = [0u8; 8];
    let mut n = 0u32;
    unsafe {
        assert_eq!(stream_read(stream, buf.as_mut_ptr() as *mut c_void, 8, &mut n), S_OK);
        assert_eq!(&buf[..n as usize], b"abcd");
        stream_release(stream);
    }
}

/// Opening the first of many small files already starts pulling the next ones; opening those later reuses them.
#[test]
fn small_files_are_pulled_ahead_of_explorer() {
    let (tx_out, rx_out) = mpsc::channel();
    let file = |name: &str, size: u64, remote: usize| OfferFile {
        name: name.into(),
        size,
        dir: false,
        mtime: None,
        ctime: None,
        atime: None,
        attrs: 0,
        remote,
    };
    let mut files: Vec<OfferFile> = (0..12).map(|i| file(&format!("s{i}.txt"), 100, i)).collect();
    files.insert(3, file("big.bin", PREFETCH_MAX_BYTES + 1, 99));
    let offer = Offer {
        from: "111".into(),
        offer_id: "of_pf".into(),
        files,
        route: Route::Local(tx_out),
        prefetch: Default::default(),
    };
    let pulls = |n: usize| -> Vec<u64> {
        (0..n)
            .map(|_| match rx_out.recv_timeout(Duration::from_secs(2)).unwrap() {
                Outgoing::Message(ClientMessage::RelayFile { payload, .. }) => payload.index.unwrap(),
                _ => panic!("unexpected message"),
            })
            .collect()
    };
    let first = open_pull(&offer, 0);
    // Entry 0 plus the two small files before the big one; nothing past the big one.
    assert_eq!(pulls(3), vec![0, 1, 2]);
    assert!(rx_out.recv_timeout(Duration::from_millis(100)).is_err(), "pulled past a big file");
    // Opening entry 1 reuses its pull and pulls nothing new (the next entry is the big one).
    let second = open_pull(&offer, 1);
    assert!(rx_out.recv_timeout(Duration::from_millis(100)).is_err(), "pulled again");
    let big = open_pull(&offer, 3);
    assert_eq!(pulls(1), vec![99]);
    // Entries after the big file are pulled ahead from there: 8 at most, entry 2 still being one of them.
    assert_eq!(pulls(PREFETCH_AHEAD - 1), (3..3 + PREFETCH_AHEAD as u64 - 1).collect::<Vec<_>>());
    assert!(rx_out.recv_timeout(Duration::from_millis(100)).is_err(), "pulled more than the window");
    unsafe {
        stream_release(first);
        stream_release(second);
        stream_release(big);
    }
    drop(offer); // forgets the streams nobody opened
    with_sinks(|sinks| assert!(sinks.is_empty(), "left {} sinks", sinks.len()));
}

#[test]
fn binary_chunks_feed_the_stream_in_order_only() {
    let (tx_out, _rx_out) = mpsc::channel();
    let offer = Offer {
        from: "111".into(),
        offer_id: "of_2".into(),
        files: vec![OfferFile {
            name: "y.bin".into(),
            size: 6,
            dir: false,
            mtime: None,
            ctime: None,
            atime: None,
            attrs: 0,
            remote: 0,
        }],
        route: Route::Local(tx_out),
        prefetch: Default::default(),
    };
    let stream = open_pull(&offer, 0);
    let tid = unsafe { (*(stream as *const PullStream)).tid.clone() };
    assert!(on_chunk("999", &tid, 0, b"abc").is_none(), "other peer must not feed the stream");
    assert!(on_chunk("111", "unknown", 0, b"abc").is_none(), "not a paste stream: normal receiver");
    on_transfer("111", FileAction::Start, &tid, &payload(json!({ "size": 6 })));
    assert_eq!(on_chunk("111", &tid, 0, b"abc").unwrap().1, None);
    on_transfer("111", FileAction::Chunk, &tid, &payload(json!({ "chunk": core_engine::codec::to_base64(b"d") })));
    assert_eq!(on_chunk("111", &tid, 4, b"ef").unwrap().1, None);
    let sha: String = Sha256::digest(b"abcdef").iter().map(|b| format!("{b:02x}")).collect();
    let (_, ack) = on_transfer("111", FileAction::End, &tid, &payload(json!({ "sha256": sha }))).unwrap();
    assert_eq!(ack, Some(true));
    let mut buf = [0u8; 8];
    let mut n = 0u32;
    unsafe {
        assert_eq!(stream_read(stream, buf.as_mut_ptr() as *mut c_void, 8, &mut n), S_OK);
        assert_eq!(&buf[..n as usize], b"abcdef");
        stream_release(stream);
    }

    // A gap (wrong offset) fails the paste.
    let stream = open_pull(&offer, 0);
    let tid = unsafe { (*(stream as *const PullStream)).tid.clone() };
    assert_eq!(on_chunk("111", &tid, 2, b"cd").unwrap().1, Some(false));
    assert!(on_chunk("111", &tid, 0, b"ab").is_none(), "failed stream is gone");
    unsafe { stream_release(stream) };
}
