package gateway

import (
	"bytes"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

func multiFixture(t *testing.T, n int) (*Gateway, *Peer, []*Peer) {
	t.Helper()
	g := NewGateway(4096, 4096, nil)
	v := &Peer{CleanID: "100000000", Multi: true, closeCh: make(chan struct{})}
	g.peers[v.CleanID] = v
	hosts := make([]*Peer, n)
	for i := range hosts {
		hosts[i] = &Peer{CleanID: "20000000" + string(rune('1'+i)), closeCh: make(chan struct{})}
		g.peers[hosts[i].CleanID] = hosts[i]
	}
	return g, v, hosts
}

func TestMultiSessionCapAndRouting(t *testing.T) {
	g, v, h := multiFixture(t, 6)
	for i := 0; i < maxViewSessions; i++ {
		if !g.setSession(v, h[i], true) {
			t.Fatalf("session %d rejected", i)
		}
	}
	if g.setSession(v, h[4], true) {
		t.Fatal("fifth concurrent session accepted")
	}
	if g.setSession(v, h[0], true) {
		t.Fatal("duplicate session to the same host accepted")
	}
	for i := 0; i < maxViewSessions; i++ {
		if g.sessionPeer(v, h[i].CleanID) != h[i] || g.sessionPeer(h[i], "") != v || g.sessionPeer(h[i], v.CleanID) != v {
			t.Fatalf("session %d does not route both ways", i)
		}
		if rcpt, view := g.sessionRoute(h[i], ""); rcpt != v || view == nil {
			t.Fatalf("host %d frames are not routed through the viewer's session queue", i)
		}
		if _, view := g.sessionRoute(v, h[i].CleanID); view != nil {
			t.Fatal("frames to a host must stay untagged")
		}
	}
	if g.sessionPeer(v, "") != nil || g.sessionPeer(v, h[4].CleanID) != nil {
		t.Fatal("multi-session viewer routed without naming a session it holds")
	}
	// A busy multi-session viewer cannot be taken as host, and its hosts stay single-session.
	legacy := &Peer{CleanID: "300000000"}
	g.peers[legacy.CleanID] = legacy
	if g.setSession(legacy, v, true) || g.setSession(legacy, h[0], true) {
		t.Fatal("busy viewer or host accepted a second controller")
	}
	if g.setSession(h[5], v, true) {
		t.Fatal("viewer with sessions became a host")
	}
}

func TestMultiSessionIsolation(t *testing.T) {
	g, v, h := multiFixture(t, 5)
	for i := 0; i < 4; i++ {
		g.setSession(v, h[i], true)
	}
	// Viewer closes one tab: only that pair ends.
	if g.closeSession(v, h[1].CleanID) != h[1] || h[1].SessionWith != "" || g.sessionPeer(v, h[1].CleanID) != nil {
		t.Fatal("closing a tab did not end its session")
	}
	for _, i := range []int{0, 2, 3} {
		if g.sessionPeer(v, h[i].CleanID) != h[i] {
			t.Fatalf("closing one tab tore down session %d", i)
		}
	}
	// Host ends its side: the viewer's other sessions survive.
	if g.closeSession(h[2], v.CleanID) != v || v.views[h[2].CleanID] != nil || g.sessionPeer(v, h[0].CleanID) != h[0] {
		t.Fatal("host-side disconnect wrong")
	}
	// A freed slot is reusable; pending sessions (Android approval) are per pair.
	if !g.setSession(v, h[4], false) || g.sessionPeer(v, h[4].CleanID) != nil {
		t.Fatal("pending session setup wrong")
	}
	if g.approveSession(v, h[0]) || !g.approveSession(v, h[4]) || g.sessionPeer(h[4], v.CleanID) != v {
		t.Fatal("approval not scoped to the pending pair")
	}
	// Host connection drops: viewer is told about that host only.
	gone, partners := g.removePeer(h[0].CleanID, h[0])
	if gone != h[0] || len(partners) != 1 || partners[0] != v || v.views[h[0].CleanID] != nil || g.sessionPeer(v, h[3].CleanID) != h[3] {
		t.Fatalf("host removal leaked into other sessions: %v", partners)
	}
	if m := sessionClosedMsg(v, h[0].CleanID, "x"); m["targetClean"] != h[0].CleanID {
		t.Fatal("multi-session viewer not told which session closed")
	}
	// Viewer connection drops: every remaining host is released and notified.
	gone, partners = g.removePeer(v.CleanID, v)
	if gone != v || len(partners) != 2 || h[3].SessionWith != "" || h[4].SessionWith != "" || len(v.views) != 0 {
		t.Fatalf("viewer removal did not release all hosts: %d", len(partners))
	}
}

func TestMultiSessionFileScoping(t *testing.T) {
	g, v, h := multiFixture(t, 3)
	g.setSession(v, h[0], true)
	g.setSession(v, h[1], true)
	if g.routeFile(v, h[0].CleanID, "start", "ft_1") != h[0] {
		t.Fatal("transfer rejected inside session")
	}
	if g.routeFile(v, h[1].CleanID, "chunk", "ft_1") != nil || g.routeFile(v, "", "chunk", "ft_1") != h[0] {
		t.Fatal("grant not bound to its pair")
	}
	if g.routeFile(h[1], v.CleanID, "ack", "ft_1") != nil || g.routeFile(h[0], v.CleanID, "ack", "ft_1") != v {
		t.Fatal("ack routed across sessions")
	}
	if g.routeFile(v, h[1].CleanID, "offer", "of_1") != h[1] || g.routeFile(h[1], v.CleanID, "pull", "of_1") != v || g.routeFile(h[2], v.CleanID, "pull", "of_1") != nil {
		t.Fatal("offer/pull not scoped to the session")
	}
	if g.routeFile(v, h[2].CleanID, "start", "ft_2") != nil {
		t.Fatal("transfer to a host without a session")
	}
}

func TestLegacyViewerStaysSingleSession(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	a := &Peer{CleanID: "100000000"}
	b := &Peer{CleanID: "200000001"}
	c := &Peer{CleanID: "200000002"}
	g.peers[a.CleanID], g.peers[b.CleanID], g.peers[c.CleanID] = a, b, c
	if !g.setSession(a, b, true) || g.setSession(a, c, true) || a.views != nil {
		t.Fatal("legacy viewer got a second session")
	}
	if rcpt, view, payload := g.binaryRoute(b, []byte{0xFF, 0xD8, 1}); rcpt != a || view != nil || len(payload) != 3 {
		t.Fatal("legacy viewer frames must stay untagged")
	}
	// A legacy sender's 0x10 binary is not parsed as a session tag.
	tagged := append([]byte{binTag}, []byte("2000000010000")...)
	if rcpt, _, payload := g.binaryRoute(a, tagged); rcpt != b || !bytes.Equal(payload, tagged) {
		t.Fatal("tag parsed for a legacy peer")
	}
	if m := sessionClosedMsg(a, b.CleanID, "x"); len(m) != 2 {
		t.Fatalf("legacy session-closed changed: %v", m)
	}
}

func TestViewQueuesAreIndependent(t *testing.T) {
	one := &viewSession{frameCh: make(chan framePacket, frameQueueLen)}
	two := &viewSession{frameCh: make(chan framePacket, frameQueueLen)}
	key := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0}
	delta := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 2, 0, 1}
	one.enqueueBinary(key)
	one.enqueueBinary(delta)
	two.enqueueBinary(key)
	two.enqueueBinary(delta)
	two.enqueueBinary(key) // restart on session two only
	if len(one.frameCh) != 2 || len(two.frameCh) != 1 {
		t.Fatalf("a restart frame leaked across sessions: %d %d", len(one.frameCh), len(two.frameCh))
	}
}

func TestMultiSessionWireFormat(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	srv := httptest.NewServer(g)
	defer srv.Close()
	url := "ws" + strings.TrimPrefix(srv.URL, "http")
	dial := func(id string, caps ...string) *websocket.Conn {
		t.Helper()
		ws, _, err := websocket.DefaultDialer.Dial(url, nil)
		if err != nil {
			t.Fatal(err)
		}
		if caps == nil {
			caps = []string{}
		}
		if err := ws.WriteJSON(map[string]interface{}{"type": "register", "deskId": id, "caps": caps}); err != nil {
			t.Fatal(err)
		}
		readJSON(t, ws, "registered-ack")
		return ws
	}
	viewer := dial("111111111", "multi-session")
	defer viewer.Close()
	h1 := dial("222222222")
	defer h1.Close()
	h2 := dial("333333333")
	defer h2.Close()
	legacyViewer := dial("444444444")
	defer legacyViewer.Close()
	h3 := dial("555555555")
	defer h3.Close()

	for _, id := range []string{"222222222", "333333333"} {
		_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": id})
		if m := readJSON(t, viewer, "session-connected"); m["targetClean"] != id || m["role"] != "viewer" {
			t.Fatalf("viewer session-connected: %v", m)
		}
	}
	readJSON(t, h1, "session-connected")
	readJSON(t, h2, "session-connected")
	_ = legacyViewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "555555555"})
	readJSON(t, legacyViewer, "session-connected")
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
	// Legacy viewer sees exactly the host's bytes.
	_ = h3.WriteMessage(websocket.BinaryMessage, key)
	if got := readBinary(t, legacyViewer); !bytes.Equal(got, key) {
		t.Fatalf("legacy viewer frame changed: % x", got)
	}
	// Host -> multi viewer JSON names its source.
	_ = h1.WriteJSON(map[string]string{"type": "relay-clipboard", "targetClean": "111111111", "text": "hi"})
	if m := readJSON(t, viewer, "clipboard-sync"); m["fromClean"] != "222222222" {
		t.Fatalf("clipboard source missing: %v", m)
	}
	// Closing one tab leaves the other streaming.
	_ = viewer.WriteJSON(map[string]string{"type": "session-disconnect", "targetClean": "222222222"})
	if m := readJSON(t, h1, "session-closed"); m["targetClean"] != nil {
		t.Fatalf("legacy host got a changed session-closed: %v", m)
	}
	_ = h2.WriteMessage(websocket.BinaryMessage, key)
	if got := readBinary(t, viewer); !bytes.Equal(got[:binTagLen], []byte("\x10333333333")) {
		t.Fatal("remaining session stopped streaming")
	}
	// A host going offline names the session; a failed connect names the target.
	h2.Close()
	if m := readJSON(t, viewer, "session-closed"); m["targetClean"] != "333333333" {
		t.Fatalf("session-closed without target: %v", m)
	}
	_ = viewer.WriteJSON(map[string]string{"type": "connect-request", "targetId": "999999999"})
	if m := readJSON(t, viewer, "error"); m["targetClean"] != "999999999" {
		t.Fatalf("connect error without target: %v", m)
	}
}

func readJSON(t *testing.T, ws *websocket.Conn, want string) map[string]interface{} {
	t.Helper()
	_ = ws.SetReadDeadline(time.Now().Add(2 * time.Second))
	for {
		var m map[string]interface{}
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
