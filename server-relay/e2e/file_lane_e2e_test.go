package e2e

import (
	"bytes"
	"fmt"
	"strings"
	"testing"

	"github.com/gorilla/websocket"
)

// openLane attaches a file lane for p (registered as id) with a token from its main connection.
func openLane(t *testing.T, r *relay, p *peer, id string) *peer {
	t.Helper()
	p.send(map[string]any{"type": "file-lane-request"})
	token, _ := p.expect("file-lane-token")["token"].(string)
	ws, _, err := websocket.DefaultDialer.Dial(r.url(), nil)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ws.Close() })
	lane := &peer{t: t, ws: ws}
	lane.send(map[string]any{"type": "file-lane-attach", "deskId": id, "token": token})
	lane.expect("file-lane-ack")
	return lane
}

// A file goes lane to lane through the real relay while video keeps flowing on the main connections, and a
// dropped lane is reported to the partner (see docs/features/relay-file-lane.md at the repository root).
func TestFileLane(t *testing.T) {
	r := startRelay(t)
	caps := map[string]any{"caps": []string{"file-lane", "file-binary", "file-ack"}}
	host := dial(t, r, hostID, caps)
	if m := host.expect("registered-ack"); !strings.Contains(fmt.Sprint(m["caps"]), "file-lane") {
		t.Fatalf("relay does not advertise file-lane: %v", m["caps"])
	}
	viewer := dial(t, r, viewerID, caps)
	viewer.expect("registered-ack")
	viewer.send(map[string]any{"type": "connect-request", "targetId": hostID})
	host.expect("session-connected")
	viewer.expect("session-connected")
	hostLane, viewerLane := openLane(t, r, host, hostID), openLane(t, r, viewer, viewerID)

	viewerLane.send(map[string]any{"type": "relay-file", "targetClean": hostID,
		"payload": map[string]any{"action": "start", "id": "ft_1", "name": "big.bin", "size": 4 << 20}})
	if m := hostLane.expect("file-transfer"); m["fromClean"] != viewerID {
		t.Fatalf("start: %v", m)
	}
	data := bytes.Repeat([]byte{0x5A}, 128<<10)
	const chunks = 32
	for i := range chunks {
		offset := uint64(i) * uint64(len(data))
		chunk := append([]byte{0x12, 4}, "ft_1"...)
		for shift := 56; shift >= 0; shift -= 8 {
			chunk = append(chunk, byte(offset>>shift))
		}
		chunk = append(chunk, data...)
		if err := viewerLane.ws.WriteMessage(websocket.BinaryMessage, append([]byte("\x10"+hostID), chunk...)); err != nil {
			t.Fatal(err)
		}
	}
	// Video on the main connection is not held up behind the transfer.
	frame := append([]byte{0xFF, 0xD8}, bytes.Repeat([]byte{0xAB}, 1024)...)
	if err := host.ws.WriteMessage(websocket.BinaryMessage, frame); err != nil {
		t.Fatal(err)
	}
	if got := viewer.expectBinary(); !bytes.Equal(got, frame) {
		t.Fatal("frame changed in transit")
	}
	for range chunks {
		if got := hostLane.expectBinary(); len(got) != 10+2+4+8+len(data) || string(got[1:10]) != viewerID {
			t.Fatalf("chunk on the lane: %d bytes", len(got))
		}
	}

	viewerLane.send(map[string]any{"type": "relay-file", "targetClean": hostID,
		"payload": map[string]any{"action": "start", "id": "ft_2", "name": "cut.bin", "size": 1}})
	hostLane.expect("file-transfer")
	_ = viewerLane.ws.Close()
	if m := host.expect("file-lane-lost"); m["fromClean"] != viewerID {
		t.Fatalf("lane lost: %v", m)
	}
}
