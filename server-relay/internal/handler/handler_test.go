package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"log/slog"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/usecase"
)

type nopStore struct{}

func (nopStore) Upsert(context.Context, model.Host) error                                { return nil }
func (nopStore) MarkOffline(context.Context, string, time.Time) error                    { return nil }
func (nopStore) MarkAllOffline(context.Context, time.Time) error                         { return nil }
func (nopStore) List(context.Context) ([]model.Host, error)                              { return nil, nil }
func (nopStore) SetAlias(context.Context, string, string) error                          { return nil }
func (nopStore) Start(context.Context, model.SessionRecord, bool) error                  { return nil }
func (nopStore) UpdateStatus(context.Context, string, string, model.SessionStatus) error { return nil }
func (nopStore) CloseForPeer(context.Context, string) error                              { return nil }
func (nopStore) CloseAllOpen(context.Context) error                                      { return nil }

func newTestHandler(relayOpts usecase.Options, opts Options) *Handler {
	log := slog.New(slog.DiscardHandler)
	m := metrics.New()
	opts.ReadBufferSize, opts.WriteBufferSize = 4096, 4096
	return New(usecase.New(nopStore{}, nopStore{}, m, relayOpts, log), m, opts, log)
}

func dialer(t *testing.T, h *Handler) func(id string, extra map[string]any) *websocket.Conn {
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	url := "ws" + strings.TrimPrefix(srv.URL, "http")
	return func(id string, extra map[string]any) *websocket.Conn {
		t.Helper()
		ws, _, err := websocket.DefaultDialer.Dial(url, nil)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { ws.Close() })
		msg := map[string]any{"type": "register", "deskId": id}
		for k, v := range extra {
			msg[k] = v
		}
		if err := ws.WriteJSON(msg); err != nil {
			t.Fatal(err)
		}
		return ws
	}
}

func TestRegistrationAndSignaling(t *testing.T) {
	creds := map[string]model.DeviceCredential{
		"123456789": {Token: strings.Repeat("a", 32)},
		"987654321": {Token: strings.Repeat("b", 32)},
		"555555555": {Token: strings.Repeat("c", 32)},
	}
	h := newTestHandler(usecase.Options{Credentials: creds}, Options{})
	dial := dialer(t, h)
	withToken := func(token string, admin bool) map[string]any {
		return map[string]any{"deviceToken": token, "isAdmin": admin, "caps": []any{"file-ack", 7}}
	}

	bad := dial("123456789", withToken("wrong", true))
	if m := readJSON(t, bad, "error"); m["message"] != msgInvalidCredentials {
		t.Fatalf("invalid registration: %v", m)
	}
	if h.relay.PeerCount() != 0 {
		t.Fatal("invalid token registered peer")
	}
	a := dial("123456789", withToken(strings.Repeat("a", 32), true))
	readJSON(t, a, "registered-ack")
	if h.relay.Lookup("123456789").IsAdmin {
		t.Fatal("client-provided admin privilege accepted")
	}
	b := dial("987654321", withToken(strings.Repeat("b", 32), false))
	readJSON(t, b, "registered-ack")
	other := dial("555555555", withToken(strings.Repeat("c", 32), false))
	readJSON(t, other, "registered-ack")

	_ = other.WriteJSON(map[string]string{"type": "p2p-signal", "targetClean": "987654321", "candidate": "evil"})
	_ = a.WriteJSON(map[string]string{"type": "connect-request", "targetId": "987 654 321"})
	m := readJSON(t, b, "session-connected")
	if caps, _ := m["peerCaps"].([]any); len(caps) != 1 || caps[0] != "file-ack" || m["role"] != "host" {
		t.Fatalf("peer capabilities not forwarded: %v", m)
	}
	readJSON(t, a, "session-connected")

	_ = a.WriteJSON(map[string]string{"type": "p2p-signal", "targetClean": "987654321", "candidate": "candidate"})
	if m := readJSON(t, b, "p2p-signal"); m["candidate"] != "candidate" || m["fromClean"] != "123456789" {
		t.Fatalf("signal routing: %v", m)
	}
	_ = a.WriteJSON(map[string]any{"type": "relay-file", "targetClean": "987654321", "payload": map[string]string{"action": "start", "id": "ft_9", "name": "a.txt"}})
	if m := readJSON(t, b, "file-transfer"); m["fromClean"] != "123456789" || m["payload"].(map[string]any)["name"] != "a.txt" {
		t.Fatalf("file routing: %v", m)
	}
	_ = b.WriteJSON(map[string]any{"type": "relay-file", "targetClean": "123456789", "payload": map[string]any{"action": "ack", "id": "ft_9", "ok": true}})
	if m := readJSON(t, a, "file-transfer"); m["fromClean"] != "987654321" {
		t.Fatalf("ack routing: %v", m)
	}
	_ = a.WriteJSON(map[string]any{"type": "relay-input", "targetClean": "987654321", "event": map[string]any{"action": "mousemove", "normX": 0.5}})
	if m := readJSON(t, b, "input-command"); m["event"].(map[string]any)["normX"] != 0.5 || m["fromClean"] != nil {
		t.Fatalf("input routing: %v", m)
	}
}

func TestMetricsRequireToken(t *testing.T) {
	h := newTestHandler(usecase.Options{}, Options{MetricsToken: strings.Repeat("m", 32)})
	for _, auth := range []string{"", "Bearer wrong"} {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest("GET", "/metrics", nil)
		req.Header.Set("Authorization", auth)
		h.ServeHTTP(rec, req)
		if rec.Code != 401 {
			t.Fatalf("metrics exposed with %q: %d", auth, rec.Code)
		}
	}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/metrics", nil)
	req.Header.Set("Authorization", "Bearer "+strings.Repeat("m", 32))
	h.ServeHTTP(rec, req)
	if rec.Code != 200 || !strings.Contains(rec.Body.String(), `"ingress"`) {
		t.Fatalf("metrics unavailable: %d %s", rec.Code, rec.Body.String())
	}
}

func TestStatusAndHealth(t *testing.T) {
	h := newTestHandler(usecase.Options{}, Options{})
	rec := httptest.NewRecorder()
	h.ServeHTTP(rec, httptest.NewRequest("GET", "/healthz", nil))
	if rec.Code != 200 || !strings.Contains(rec.Body.String(), `"ok"`) {
		t.Fatalf("health: %d %s", rec.Code, rec.Body.String())
	}
	rec = httptest.NewRecorder()
	h.ServeHTTP(rec, httptest.NewRequest("GET", "/", nil))
	var status dto.Status
	if err := json.Unmarshal(rec.Body.Bytes(), &status); err != nil || status.Service != "opendesk.relay" {
		t.Fatalf("status: %v %s", err, rec.Body.String())
	}
}

func TestMultiSessionWireFormat(t *testing.T) {
	dial := dialer(t, newTestHandler(usecase.Options{}, Options{}))
	register := func(id string, caps ...string) *websocket.Conn {
		t.Helper()
		if caps == nil {
			caps = []string{}
		}
		ws := dial(id, map[string]any{"caps": caps})
		readJSON(t, ws, "registered-ack")
		return ws
	}
	viewer := register("111111111", "multi-session")
	h1 := register("222222222")
	h2 := register("333333333")
	singleViewer := register("444444444")
	h3 := register("555555555")

	for _, id := range []string{"222222222", "333333333"} {
		_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": id})
		if m := readJSON(t, viewer, "session-connected"); m["targetClean"] != id || m["role"] != "viewer" {
			t.Fatalf("viewer session-connected: %v", m)
		}
	}
	readJSON(t, h1, "session-connected")
	readJSON(t, h2, "session-connected")
	_ = singleViewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "555555555"})
	readJSON(t, singleViewer, "session-connected")
	readJSON(t, h3, "session-connected")

	key := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0, 0, 7}
	_ = h1.WriteMessage(websocket.BinaryMessage, key)
	if got := readBinary(t, viewer); !bytes.Equal(got, append([]byte("\x10222222222"), key...)) {
		t.Fatalf("video not tagged with its host: % x", got)
	}
	audio := []byte{0x02, 48, 1, 2}
	_ = h2.WriteMessage(websocket.BinaryMessage, audio)
	if got := readBinary(t, viewer); !bytes.Equal(got, append([]byte("\x10333333333"), audio...)) {
		t.Fatalf("audio not tagged with its host: % x", got)
	}
	// Viewer -> host: the tag selects the host and is stripped.
	_ = viewer.WriteMessage(websocket.BinaryMessage, append([]byte("\x10333333333"), audio...))
	if got := readBinary(t, h2); !bytes.Equal(got, audio) {
		t.Fatalf("host received % x", got)
	}
	// A single-session viewer sees exactly the host's bytes.
	_ = h3.WriteMessage(websocket.BinaryMessage, key)
	if got := readBinary(t, singleViewer); !bytes.Equal(got, key) {
		t.Fatalf("single-session viewer frame changed: % x", got)
	}
	// Host -> multi viewer JSON names its source.
	_ = h1.WriteJSON(map[string]string{"type": "relay-clipboard", "targetClean": "111111111", "text": "hi"})
	if m := readJSON(t, viewer, "clipboard-sync"); m["fromClean"] != "222222222" || m["text"] != "hi" {
		t.Fatalf("clipboard source missing: %v", m)
	}
	// Closing one tab leaves the other streaming; the single-session host gets the unchanged message shape.
	_ = viewer.WriteJSON(map[string]string{"type": "session-disconnect", "targetClean": "222222222"})
	if m := readJSON(t, h1, "session-closed"); m["targetClean"] != nil || m["message"] != msgEndedByPeer {
		t.Fatalf("single-session host got a changed session-closed: %v", m)
	}
	_ = h2.WriteMessage(websocket.BinaryMessage, key)
	if got := readBinary(t, viewer); !bytes.Equal(got[:10], []byte("\x10333333333")) {
		t.Fatal("remaining session stopped streaming")
	}
	// A host going offline names the session; a failed connect names the target.
	h2.Close()
	if m := readJSON(t, viewer, "session-closed"); m["targetClean"] != "333333333" {
		t.Fatalf("session-closed without target: %v", m)
	}
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "999999999"})
	if m := readJSON(t, viewer, "error"); m["targetClean"] != "999999999" || m["message"] != "Device 999 999 999 was not found or is offline." {
		t.Fatalf("connect error without target: %v", m)
	}
}

func readJSON(t *testing.T, ws *websocket.Conn, want string) map[string]any {
	t.Helper()
	_ = ws.SetReadDeadline(time.Now().Add(2 * time.Second))
	for {
		var m map[string]any
		if err := ws.ReadJSON(&m); err != nil {
			t.Fatalf("waiting for %s: %v", want, err)
		}
		if m["type"] == want {
			return m
		}
	}
}

func readBinary(t *testing.T, ws *websocket.Conn) []byte {
	t.Helper()
	_ = ws.SetReadDeadline(time.Now().Add(2 * time.Second))
	for {
		kind, data, err := ws.ReadMessage()
		if err != nil {
			t.Fatalf("waiting for binary: %v", err)
		}
		if kind == websocket.BinaryMessage {
			return data
		}
	}
}
