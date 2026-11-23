package handler

import (
	"bytes"
	"fmt"
	"net/http/httptest"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/usecase"
)

// laneClient opens raw WebSocket connections to one relay.
type laneClient struct {
	t   *testing.T
	url string
}

func newLaneClient(t *testing.T, h *Handler) laneClient {
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	return laneClient{t: t, url: "ws" + strings.TrimPrefix(srv.URL, "http")}
}

func (c laneClient) open() *websocket.Conn {
	c.t.Helper()
	ws, _, err := websocket.DefaultDialer.Dial(c.url, nil)
	if err != nil {
		c.t.Fatal(err)
	}
	c.t.Cleanup(func() { ws.Close() })
	return ws
}

// fullCaps is a capability list as long as the relay keeps (a Windows admin build sends 32 names), file-binary
// among them.
func fullCaps() []string {
	caps := []string{"file-binary"}
	for i := len(caps); i < 32; i++ {
		caps = append(caps, fmt.Sprintf("cap-%d", i))
	}
	return caps
}

// register opens a main connection for id with a full capability list and fileLane set.
func (c laneClient) register(id string) *websocket.Conn {
	c.t.Helper()
	ws := c.open()
	_ = ws.WriteJSON(map[string]any{"type": "register", "deskId": id, "caps": fullCaps(), "fileLane": true})
	if ack := readJSON(c.t, ws, "registered-ack"); !slices.Contains(ack["caps"].([]any), any("file-lane")) {
		c.t.Fatalf("relay does not advertise file-lane: %v", ack["caps"])
	}
	return ws
}

// lane asks main for a token and attaches a lane with it.
func (c laneClient) lane(main *websocket.Conn, id string) *websocket.Conn {
	c.t.Helper()
	_ = main.WriteJSON(map[string]string{"type": "file-lane-request"})
	token, _ := readJSON(c.t, main, "file-lane-token")["token"].(string)
	ws := c.open()
	_ = ws.WriteJSON(map[string]string{"type": "file-lane-attach", "deskId": id, "token": token})
	readJSON(c.t, ws, "file-lane-ack")
	return ws
}

// A lane attaches only with its device's token, carries a transfer from lane to lane, and when it drops the
// partner hears which transfers it cut.
func TestFileLaneOnTheWire(t *testing.T) {
	c := newLaneClient(t, newTestHandler(usecase.Options{}, Options{}))
	viewer, host := c.register("111111111"), c.register("222222222")

	// A stranger guessing, or a token presented for another device, is refused and disconnected.
	_ = viewer.WriteJSON(map[string]string{"type": "file-lane-request"})
	token, _ := readJSON(t, viewer, "file-lane-token")["token"].(string)
	for _, try := range []map[string]string{
		{"type": "file-lane-attach", "deskId": "111111111", "token": "guess"},
		{"type": "file-lane-attach", "deskId": "222222222", "token": token},
	} {
		bad := c.open()
		_ = bad.WriteJSON(try)
		if m := readJSON(t, bad, "error"); m["message"] != msgLaneRefused {
			t.Fatalf("refusal: %v", m)
		}
		_ = bad.SetReadDeadline(time.Now().Add(2 * time.Second))
		if _, _, err := bad.ReadMessage(); err == nil {
			t.Fatal("refused lane left open")
		}
	}

	viewerLane, hostLane := c.lane(viewer, "111111111"), c.lane(host, "222222222")
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "222222222"})
	// A full list does not crowd the lane out: the partner sees it after the 32 names it sent.
	for _, ws := range []*websocket.Conn{viewer, host} {
		caps, _ := readJSON(t, ws, "session-connected")["peerCaps"].([]any)
		if len(caps) != 33 || caps[32] != "file-lane" {
			t.Fatalf("peerCaps %v", caps)
		}
	}

	_ = viewerLane.WriteJSON(map[string]any{"type": "relay-file", "targetClean": "222222222",
		"payload": map[string]any{"action": "start", "id": "ft_1", "name": "a.bin", "size": 3}})
	if m := readJSON(t, hostLane, "file-transfer"); m["fromClean"] != "111111111" {
		t.Fatalf("start on the lane: %v", m)
	}
	chunk := append([]byte{0x12, 4}, "ft_1\x00\x00\x00\x00\x00\x00\x00\x00abc"...)
	_ = viewerLane.WriteMessage(websocket.BinaryMessage, append([]byte("\x10222222222"), chunk...))
	if got := readBinary(t, hostLane); !bytes.Equal(got, append([]byte("\x10111111111"), chunk...)) {
		t.Fatalf("chunk on the lane: % x", got)
	}
	// The verdict goes back on the main connections.
	_ = host.WriteJSON(map[string]any{"type": "relay-file", "targetClean": "111111111",
		"payload": map[string]any{"action": "ack", "id": "ft_1", "ok": true}})
	readJSON(t, viewer, "file-transfer")

	// A second transfer is cut when the sending lane drops; the receiver learns on its main connection.
	_ = viewerLane.WriteJSON(map[string]any{"type": "relay-file", "targetClean": "222222222",
		"payload": map[string]any{"action": "start", "id": "ft_2", "name": "b.bin", "size": 3}})
	readJSON(t, hostLane, "file-transfer")
	_ = viewerLane.Close()
	m := readJSON(t, host, "file-lane-lost")
	if ids, _ := m["ids"].([]any); m["fromClean"] != "111111111" || len(ids) != 1 || ids[0] != "ft_2" {
		t.Fatalf("lane lost: %v", m)
	}
}
