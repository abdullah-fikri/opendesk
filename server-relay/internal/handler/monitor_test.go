package handler

import (
	"bytes"
	"encoding/json"
	"slices"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/usecase"
)

// readUntil reads ws up to a text message of type want and returns the types seen before it ("binary" for
// binary messages).
func readUntil(t *testing.T, ws *websocket.Conn, want string) []string {
	t.Helper()
	_ = ws.SetReadDeadline(time.Now().Add(2 * time.Second))
	var seen []string
	for {
		kind, data, err := ws.ReadMessage()
		if err != nil {
			t.Fatalf("waiting for %s: %v (seen %v)", want, err, seen)
		}
		if kind == websocket.BinaryMessage {
			seen = append(seen, "binary")
			continue
		}
		var m map[string]any
		_ = json.Unmarshal(data, &m)
		if m["type"] == want {
			return seen
		}
		seen = append(seen, m["type"].(string))
	}
}

func TestMonitorWallWireFormat(t *testing.T) {
	dial := dialer(t, newTestHandler(usecase.Options{}, Options{}))
	register := func(id string, admin bool, caps ...string) *websocket.Conn {
		t.Helper()
		if caps == nil {
			caps = []string{}
		}
		ws := dial(id, map[string]any{"isAdmin": admin, "caps": caps})
		if ack := readJSON(t, ws, "registered-ack"); !slices.Contains(ack["caps"].([]any), any("monitor")) {
			t.Fatalf("relay does not advertise monitor: %v", ack)
		}
		return ws
	}
	admin := register("111111111", true, "multi-session", "monitor-view")
	host := register("222222222", false, "monitor-host")
	legacy := register("333333333", false)

	_ = admin.WriteJSON(map[string]any{"type": "monitor-set", "targets": []string{"222 222 222", "333333333", "444444444"}})
	if m := readJSON(t, host, "monitor-start"); m["maxWidth"] != float64(640) || m["fps"] != float64(2) {
		t.Fatalf("monitor-start: %v", m)
	}
	got := map[any]any{}
	for range 3 {
		m := readJSON(t, admin, "monitor-state")
		got[m["targetClean"]] = m["state"]
	}
	if got["222222222"] != "live" || got["333333333"] != "unsupported" || got["444444444"] != "offline" {
		t.Fatalf("monitor states: %v", got)
	}

	jpeg := []byte{0xFF, 0xD8, 0xFF, 0xE0, 9}
	_ = host.WriteMessage(websocket.BinaryMessage, append([]byte{0x11}, jpeg...))
	if b := readBinary(t, admin); !bytes.Equal(b, append([]byte("\x11222222222"), jpeg...)) {
		t.Fatalf("thumbnail not tagged: % x", b)
	}

	// Read-only: nothing the administrator sends reaches a watched host. The relay handles one connection's
	// messages in order, so anything forwarded would arrive before the monitor-stop that follows.
	_ = admin.WriteJSON(map[string]any{"type": "relay-input", "targetClean": "222222222", "event": map[string]any{"action": "keydown", "keyCode": 65}})
	_ = admin.WriteJSON(map[string]any{"type": "relay-clipboard", "targetClean": "222222222", "text": "x"})
	_ = admin.WriteJSON(map[string]any{"type": "relay-file", "targetClean": "222222222", "payload": map[string]string{"action": "start", "id": "ft_1", "name": "a"}})
	_ = admin.WriteJSON(map[string]any{"type": "p2p-signal", "targetClean": "222222222", "candidate": "c"})
	_ = admin.WriteMessage(websocket.BinaryMessage, append([]byte("\x10222222222"), 0x02, 48, 1, 2))
	_ = admin.WriteMessage(websocket.BinaryMessage, append([]byte{0x11}, jpeg...))
	_ = admin.WriteJSON(map[string]any{"type": "monitor-set", "targets": []string{}})
	if seen := readUntil(t, host, "monitor-stop"); len(seen) != 0 {
		t.Fatalf("administrator traffic reached the watched host: %v", seen)
	}

	// A device without the admin role is refused, and the host never hears of it.
	intruder := register("555555555", false, "multi-session", "monitor-view")
	_ = intruder.WriteJSON(map[string]any{"type": "monitor-set", "targets": []string{"222222222"}})
	if m := readJSON(t, intruder, "monitor-state"); m["state"] != "denied" {
		t.Fatalf("non-administrator not denied: %v", m)
	}
	// Watching again restarts the stream; the administrator closing its connection stops it.
	_ = admin.WriteJSON(map[string]any{"type": "monitor-set", "targets": []string{"222222222"}})
	if seen := readUntil(t, host, "monitor-start"); len(seen) != 0 {
		t.Fatalf("host got %v before monitor-start", seen)
	}
	admin.Close()
	readJSON(t, host, "monitor-stop")

	// Sessions are unaffected, and neither side of this one saw monitor traffic: not the intruder (no
	// thumbnails), not the host without monitor-host although it was on the wall (no monitor-start).
	_ = intruder.WriteJSON(map[string]string{"type": "connect-request", "targetId": "333333333"})
	if seen := readUntil(t, intruder, "session-connected"); len(seen) != 0 {
		t.Fatalf("intruder saw %v", seen)
	}
	if seen := readUntil(t, legacy, "session-connected"); len(seen) != 0 {
		t.Fatalf("host without monitor-host saw %v", seen)
	}
}
