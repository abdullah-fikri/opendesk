package handler

import (
	"bytes"
	"slices"
	"testing"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/usecase"
)

// Binary file chunks on the wire: the relay advertises file-binary, forwards a granted chunk tagged with its
// sender (whatever the receiver's session mode) and drops chunks of foreign transfers.
func TestBinaryFileChunkWireFormat(t *testing.T) {
	dial := dialer(t, newTestHandler(usecase.Options{}, Options{}))
	register := func(id string, caps ...string) *websocket.Conn {
		t.Helper()
		ws := dial(id, map[string]any{"caps": append([]string{}, caps...)})
		ack := readJSON(t, ws, "registered-ack")
		relayCaps, _ := ack["caps"].([]any)
		if !slices.Contains(relayCaps, any("file-binary")) {
			t.Fatalf("relay does not advertise file-binary: %v", ack["caps"])
		}
		return ws
	}
	viewer := register("111111111", "multi-session")
	host := register("222222222", "file-binary")
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "222222222"})
	readJSON(t, viewer, "session-connected")
	readJSON(t, host, "session-connected")

	chunk := func(id string, data string) []byte {
		msg := append([]byte{0x12, byte(len(id))}, id...)
		return append(append(msg, 0, 0, 0, 0, 0, 0, 0, 0), data...)
	}
	_ = viewer.WriteJSON(map[string]any{"type": "relay-file", "targetClean": "222222222",
		"payload": map[string]any{"action": "start", "id": "ft_1", "name": "a.txt", "size": 3}})
	readJSON(t, host, "file-transfer")
	_ = viewer.WriteMessage(websocket.BinaryMessage, append([]byte("\x10222222222"), chunk("ft_2", "bad")...))
	_ = viewer.WriteMessage(websocket.BinaryMessage, append([]byte("\x10222222222"), chunk("ft_1", "abc")...))
	if got := readBinary(t, host); !bytes.Equal(got, append([]byte("\x10111111111"), chunk("ft_1", "abc")...)) {
		t.Fatalf("host received % x", got)
	}
}
