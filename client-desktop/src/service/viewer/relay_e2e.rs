//! End to end against a running relay: `cargo test --release relay_e2e -- --ignored --nocapture`
//! (`OPENDESK_E2E_RELAY`, default ws://127.0.0.1:8088). Two simulated hosts stream real key frames to one
//! multi-session viewer, which demultiplexes and decodes them per tab; a single-session viewer stays untagged.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::domain::capability::MULTI_SESSION as CAP;
use crate::protocol::InputEvent;
use crate::protocol::binary::{tag as tag_binary, untag as untag_binary};
use crate::service::viewer::decoder::{ViewerDecoder, ViewerOutput};
use crate::service::viewer::registry::{Open, Registry};

fn caps_has(caps: &Value, cap: &str) -> bool {
    caps.as_array().is_some_and(|l| l.iter().any(|c| c.as_str() == Some(cap)))
}

fn parse_monitor_count(event: &Value) -> usize {
    match InputEvent::parse(event) {
        Some(InputEvent::Monitors { list, .. }) => list.len(),
        _ => 0,
    }
}

mod tests {
    use super::*;
    use core_engine::stream::{FrameInput, StreamEncoder};
    use std::net::TcpStream;
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Message, WebSocket};

    type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

    fn ids() -> [String; 4] {
        // 9 digits, no leading zero, unlikely to collide with real devices on the same relay.
        let base = 700_000_000 + (std::process::id() % 50_000) * 1000;
        [0, 1, 2, 3].map(|i| (base + i).to_string())
    }

    fn connect(id: &str, caps: &[&str]) -> (Ws, Value) {
        let url = std::env::var("OPENDESK_E2E_RELAY").unwrap_or_else(|_| "ws://127.0.0.1:8088".into());
        let (mut ws, _) = tungstenite::connect(url.as_str()).expect("relay reachable");
        if let MaybeTlsStream::Plain(s) = ws.get_mut() {
            s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        }
        let reg = json!({ "type": "register", "deskId": id, "platform": "windows-pc", "deviceName": format!("e2e-{id}"), "deviceToken": "", "isAdmin": false, "caps": caps });
        ws.send(Message::Text(reg.to_string().into())).unwrap();
        let ack = wait_json(&mut ws, |v| v["type"] == "registered-ack" || v["type"] == "error");
        assert_eq!(ack["type"], "registered-ack", "register {id}: {ack}");
        (ws, ack)
    }

    fn read(ws: &mut Ws) -> Option<Message> {
        match ws.read() {
            Ok(m) => Some(m),
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                None
            }
            Err(e) => panic!("socket: {e}"),
        }
    }

    fn wait_json(ws: &mut Ws, want: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(Message::Text(t)) = read(ws) {
                let v: Value = serde_json::from_str(&t).unwrap();
                if want(&v) {
                    return v;
                }
            }
        }
        panic!("timed out waiting for a message");
    }

    fn wait_binary(ws: &mut Ws) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Some(Message::Binary(b)) = read(ws) {
                return b.to_vec();
            }
        }
        panic!("timed out waiting for a binary message");
    }

    /// A solid-colour key frame, as a host's capture loop would send it.
    fn key_frame(rgb: u32) -> Vec<u8> {
        let (w, h) = (64usize, 48usize);
        let px = vec![rgb; w * h];
        StreamEncoder::new(w as i32, h as i32, 85, true)
            .encode(&FrameInput { pixels: &px, stride: w, dirty_hint: None, moves: &[] })
            .unwrap()
    }

    #[test]
    #[ignore]
    fn relay_e2e_two_hosts_one_multi_session_viewer() {
        let [viewer_id, a_id, b_id, legacy_id] = ids();
        let host_caps = ["file-ack", "file-offer", "cursor-shape", "video-delta"];
        let (mut host_a, _) = connect(&a_id, &host_caps);
        let (mut host_b, _) = connect(&b_id, &host_caps);
        let (mut viewer, ack) = connect(&viewer_id, &["file-ack", "video-delta", CAP]);
        if !caps_has(&ack["caps"], CAP) {
            eprintln!("relay lacks multi-session; skipping (legacy behaviour is the single-session path)");
            return;
        }
        let reg = Registry::new();
        reg.set_multi(true);

        for host in [&a_id, &b_id] {
            viewer
                .send(Message::Text(json!({ "type": "connect-request", "targetId": host }).to_string().into()))
                .unwrap();
            let sc = wait_json(&mut viewer, |v| v["type"] == "session-connected");
            assert_eq!(sc["role"], "viewer");
            let Open::Opened(s) = reg.open(sc["targetClean"].as_str().unwrap(), 1, 0, 2) else { panic!() };
            s.connected.store(true, Ordering::SeqCst);
        }
        for host in [&mut host_a, &mut host_b] {
            let sc = wait_json(host, |v| v["type"] == "session-connected");
            assert_eq!(sc["role"], "host");
            assert_eq!(sc["targetClean"], viewer_id.as_str());
        }
        reg.set_focus(&a_id);
        reg.set_layout(1); // split view: both decoded

        // Both hosts stream; the viewer demultiplexes by tag and decodes each tab separately.
        host_a.send(Message::Binary(key_frame(0x00FF_0000).into())).unwrap();
        host_b.send(Message::Binary(key_frame(0x0000_00FF).into())).unwrap();
        let mut decoders = std::collections::HashMap::new();
        for _ in 0..2 {
            let bin = wait_binary(&mut viewer);
            let (host, payload) = untag_binary(&bin).expect("multi-session frames are tagged");
            let s = reg.get(host).expect("frame for an open tab");
            s.inbox.push(payload.to_vec());
            let out = decoders.entry(host.to_string()).or_insert_with(ViewerDecoder::new).process(s.inbox.drain());
            let ViewerOutput::Frame(buf, w, h) = out else { panic!("frame from {host} did not decode") };
            assert_eq!((w, h), (64, 48));
            let px = buf.as_bytes();
            let (r, b) = (px[0], px[2]);
            if host == a_id {
                assert!(r > 200 && b < 60, "A is red: {r},{b}");
            } else {
                assert!(b > 200 && r < 60, "B is blue: {r},{b}");
            }
        }
        assert_eq!(decoders.len(), 2, "one frame per host");

        // Viewer -> host: text needs targetClean, binary (voice) needs the tag; the host sees legacy bytes.
        viewer
            .send(Message::Text(
                json!({ "type": "relay-input", "targetClean": b_id, "event": { "action": "video-keyframe" } })
                    .to_string()
                    .into(),
            ))
            .unwrap();
        let cmd = wait_json(&mut host_b, |v| v["type"] == "input-command");
        assert_eq!(cmd["event"]["action"], "video-keyframe");
        let voice = [0x02u8, 48, 1, 0, 2, 0];
        viewer.send(Message::Binary(tag_binary(&a_id, &voice).unwrap().into())).unwrap();
        assert_eq!(wait_binary(&mut host_a), voice.to_vec(), "relay strips the tag for the host");

        // Host -> multi-session viewer text carries fromClean; the monitors announcement parses.
        let mons = [core_engine::capture::MonitorBounds { left: 0, top: 0, width: 1920, height: 1080 }];
        host_b.send(Message::Text(json!({ "type": "relay-input", "targetClean": viewer_id, "event": serde_json::to_value(crate::service::host::capture::monitors_event(&mons, 0)).unwrap() }).to_string().into())).unwrap();
        let ev = wait_json(&mut viewer, |v| v["type"] == "input-command");
        assert_eq!(ev["fromClean"], b_id.as_str());
        assert_eq!(parse_monitor_count(&ev["event"]), 1);

        // Closing one tab leaves the other streaming.
        viewer
            .send(Message::Text(json!({ "type": "session-disconnect", "targetClean": a_id }).to_string().into()))
            .unwrap();
        wait_json(&mut host_a, |v| v["type"] == "session-closed");
        reg.remove(&a_id);
        host_b.send(Message::Binary(key_frame(0x0000_00FF).into())).unwrap();
        let bin = wait_binary(&mut viewer);
        assert_eq!(untag_binary(&bin).unwrap().0, b_id);

        // A host ending its side names itself in session-closed.
        host_b
            .send(Message::Text(json!({ "type": "session-disconnect", "targetClean": viewer_id }).to_string().into()))
            .unwrap();
        let closed = wait_json(&mut viewer, |v| v["type"] == "session-closed");
        assert_eq!(closed["targetClean"], b_id.as_str());

        // Legacy viewer (no cap) on the same relay: untagged frames, no fromClean.
        let (mut legacy, _) = connect(&legacy_id, &["file-ack", "video-delta"]);
        legacy.send(Message::Text(json!({ "type": "connect-request", "targetId": a_id }).to_string().into())).unwrap();
        wait_json(&mut legacy, |v| v["type"] == "session-connected");
        wait_json(&mut host_a, |v| v["type"] == "session-connected");
        let frame = key_frame(0x0000_FF00);
        host_a.send(Message::Binary(frame.clone().into())).unwrap();
        let bin = wait_binary(&mut legacy);
        assert!(untag_binary(&bin).is_none());
        assert_eq!(bin, frame, "legacy viewers get the exact host bytes");
        host_a.send(Message::Text(json!({ "type": "relay-input", "targetClean": legacy_id, "event": { "action": "cursor-shape", "shape": 2 } }).to_string().into())).unwrap();
        assert!(wait_json(&mut legacy, |v| v["type"] == "input-command").get("fromClean").is_none());
    }
}
