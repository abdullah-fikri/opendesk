package gateway

import (
	"encoding/json"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
)

func BenchmarkRelayRouting(b *testing.B) {
	g := NewGateway(4096, 4096, nil)
	a := &Peer{CleanID: "111"}
	peer := &Peer{CleanID: "222"}
	g.peers[a.CleanID], g.peers[peer.CleanID] = a, peer
	if !g.setSession(a, peer, true) {
		b.Fatal("session setup failed")
	}
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if g.sessionPeer(a, peer.CleanID) != peer {
			b.Fatal("routing failed")
		}
	}
}

func BenchmarkRelayInputJSON(b *testing.B) {
	msg := []byte(`{"type":"relay-input","targetClean":"222","event":{"type":"mouse-move","x":960,"y":540}}`)
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		var raw map[string]interface{}
		if err := json.Unmarshal(msg, &raw); err != nil {
			b.Fatal(err)
		}
	}
}

func BenchmarkRelayFileJSON(b *testing.B) {
	msg := []byte(`{"type":"relay-file","targetClean":"222","payload":{"action":"chunk","id":"ft_123","chunk":"YWJjZA=="}}`)
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		var raw map[string]interface{}
		if err := json.Unmarshal(msg, &raw); err != nil {
			b.Fatal(err)
		}
	}
}

func TestWebSocketRegistrationAndSignaling(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	g.credentials["123456789"] = deviceCredential{Token: strings.Repeat("a", 32), Admin: false}
	g.credentials["987654321"] = deviceCredential{Token: strings.Repeat("b", 32)}
	g.credentials["555555555"] = deviceCredential{Token: strings.Repeat("c", 32)}
	srv := httptest.NewServer(g)
	defer srv.Close()
	url := "ws" + strings.TrimPrefix(srv.URL, "http")
	dial := func(id, token string, admin bool) *websocket.Conn {
		t.Helper()
		ws, _, err := websocket.DefaultDialer.Dial(url, nil)
		if err != nil {
			t.Fatal(err)
		}
		if err := ws.WriteJSON(map[string]interface{}{"type": "register", "deskId": id, "deviceToken": token, "isAdmin": admin, "caps": []string{"file-ack"}}); err != nil {
			t.Fatal(err)
		}
		return ws
	}
	bad := dial("123456789", "wrong", true)
	var reply map[string]interface{}
	if err := bad.ReadJSON(&reply); err != nil || reply["type"] != "error" {
		t.Fatalf("invalid registration: %v %v", reply, err)
	}
	bad.Close()
	if g.countPeers() != 0 {
		t.Fatal("invalid token registered peer")
	}
	a := dial("123456789", strings.Repeat("a", 32), true)
	defer a.Close()
	if err := a.ReadJSON(&reply); err != nil || reply["type"] != "registered-ack" {
		t.Fatalf("valid registration: %v %v", reply, err)
	}
	if g.getPeer("123456789").IsAdmin {
		t.Fatal("client-provided admin privilege accepted")
	}
	b := dial("987654321", strings.Repeat("b", 32), false)
	defer b.Close()
	if err := b.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	other := dial("555555555", strings.Repeat("c", 32), false)
	defer other.Close()
	if err := other.ReadJSON(&reply); err != nil {
		t.Fatal(err)
	}
	if err := other.WriteJSON(map[string]string{"type": "p2p-signal", "targetClean": "987654321", "candidate": "evil"}); err != nil {
		t.Fatal(err)
	}
	if err := a.WriteJSON(map[string]string{"type": "connect-request", "targetId": "987654321"}); err != nil {
		t.Fatal(err)
	}
	if err := b.ReadJSON(&reply); err != nil || reply["type"] != "session-connected" {
		t.Fatalf("host connection: %v %v", reply, err)
	}
	if caps, _ := reply["peerCaps"].([]interface{}); len(caps) != 1 || caps[0] != "file-ack" {
		t.Fatalf("peer capabilities not forwarded: %v", reply)
	}
	if err := a.ReadJSON(&reply); err != nil || reply["type"] != "session-connected" {
		t.Fatalf("viewer connection: %v %v", reply, err)
	}
	if err := a.WriteJSON(map[string]string{"type": "p2p-signal", "targetClean": "987654321", "candidate": "candidate"}); err != nil {
		t.Fatal(err)
	}
	b.SetReadDeadline(time.Now().Add(time.Second))
	if err := b.ReadJSON(&reply); err != nil || reply["type"] != "p2p-signal" || reply["candidate"] != "candidate" || reply["fromClean"] != "123456789" {
		t.Fatalf("signal routing: %v %v", reply, err)
	}
	if err := a.WriteJSON(map[string]interface{}{"type": "relay-file", "targetClean": "987654321", "payload": map[string]string{"action": "start", "id": "ft_9", "name": "a.txt"}}); err != nil {
		t.Fatal(err)
	}
	if err := b.ReadJSON(&reply); err != nil || reply["type"] != "file-transfer" || reply["fromClean"] != "123456789" {
		t.Fatalf("file routing: %v %v", reply, err)
	}
	if err := b.WriteJSON(map[string]interface{}{"type": "relay-file", "targetClean": "123456789", "payload": map[string]interface{}{"action": "ack", "id": "ft_9", "ok": true}}); err != nil {
		t.Fatal(err)
	}
	a.SetReadDeadline(time.Now().Add(time.Second))
	if err := a.ReadJSON(&reply); err != nil || reply["type"] != "file-transfer" || reply["fromClean"] != "987654321" {
		t.Fatalf("ack routing: %v %v", reply, err)
	}
}

func TestDeviceAuthentication(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	if ok, admin := g.authenticate("123456789", "", true); !ok || !admin {
		t.Fatal("open relay (no credentials configured) must admit devices with their claimed role")
	}
	g.credentials["123456789"] = deviceCredential{Token: "0123456789abcdef0123456789abcdef", Admin: true}
	for _, input := range []struct {
		id, token string
	}{
		{"123456789", ""},
		{"123456789", "wrong"},
		{"987654321", "0123456789abcdef0123456789abcdef"},
	} {
		if ok, _ := g.authenticate(input.id, input.token, true); ok {
			t.Fatalf("accepted invalid credential for %s", input.id)
		}
	}
	if ok, admin := g.authenticate("123456789", "0123456789abcdef0123456789abcdef", false); !ok || !admin {
		t.Fatal("rejected provisioned administrator")
	}
}

func TestFileGrantOutlivesSessionOnly(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	a := &Peer{CleanID: "111"}
	b := &Peer{CleanID: "222"}
	other := &Peer{CleanID: "333"}
	g.peers[a.CleanID], g.peers[b.CleanID], g.peers[other.CleanID] = a, b, other

	if g.routeFile(a, b.CleanID, "start", "ft_1") != nil {
		t.Fatal("transfer started without session")
	}
	if !g.setSession(a, b, true) || g.routeFile(a, b.CleanID, "start", "ft_1") != b {
		t.Fatal("transfer rejected inside session")
	}
	if g.routeFile(a, b.CleanID, "start", "ft_1") != nil {
		t.Fatal("duplicate transfer id accepted")
	}
	g.closeSession(a, b.CleanID)
	if g.routeFile(a, b.CleanID, "chunk", "ft_1") != b || g.routeFile(a, "", "end", "ft_1") != b {
		t.Fatal("granted transfer stopped after session ended")
	}
	if g.routeFile(other, b.CleanID, "chunk", "ft_1") != nil || g.routeFile(a, other.CleanID, "chunk", "ft_1") != nil || g.routeFile(a, b.CleanID, "chunk", "ft_2") != nil {
		t.Fatal("grant reused by another peer, target or transfer")
	}
	if g.routeFile(other, a.CleanID, "ack", "ft_1") != nil || g.routeFile(b, a.CleanID, "ack", "ft_1") != a {
		t.Fatal("ack routed to wrong peer")
	}
	if g.routeFile(a, b.CleanID, "chunk", "ft_1") != nil || g.routeFile(b, a.CleanID, "ack", "ft_1") != nil {
		t.Fatal("grant survived ack")
	}

	if !g.setSession(a, b, true) || g.routeFile(a, b.CleanID, "start", "ft_3") != b {
		t.Fatal("second transfer rejected")
	}
	g.peers[b.CleanID] = &Peer{CleanID: b.CleanID} // receiver reconnected
	if g.routeFile(a, b.CleanID, "chunk", "ft_3") != nil {
		t.Fatal("grant followed a replaced connection")
	}
	g.fileGrants[grantKey(a.CleanID, "ft_4")] = &fileGrant{from: a, to: g.peers[b.CleanID], expires: time.Now().Add(-time.Second)}
	if g.routeFile(a, b.CleanID, "chunk", "ft_4") != nil {
		t.Fatal("expired grant accepted")
	}
}

func TestClipboardOfferNeedsSession(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	a := &Peer{CleanID: "111"}
	b := &Peer{CleanID: "222"}
	other := &Peer{CleanID: "333"}
	g.peers[a.CleanID], g.peers[b.CleanID], g.peers[other.CleanID] = a, b, other

	if g.routeFile(a, b.CleanID, "offer", "of_1") != nil {
		t.Fatal("offer routed without session")
	}
	g.setSession(a, b, true)
	if g.routeFile(a, b.CleanID, "offer", "of_1") != b || g.routeFile(b, a.CleanID, "pull", "of_1") != a {
		t.Fatal("offer/pull rejected inside session")
	}
	if g.routeFile(other, a.CleanID, "pull", "of_1") != nil {
		t.Fatal("pull accepted from outside the session")
	}
	g.closeSession(a, b.CleanID)
	if g.routeFile(b, a.CleanID, "pull", "of_1") != nil {
		t.Fatal("pull outlived the session")
	}
}

func TestFrameQueueKeepsDeltaOrderAndRestartsCleanly(t *testing.T) {
	p := &Peer{frameCh: make(chan framePacket, frameQueueLen)}
	key := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0}
	delta := func(n byte) []byte { return []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 2, 0, n} }
	idr := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 3, 1}
	if !isRestartFrame([]byte{0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0}) || !isRestartFrame(key) || !isRestartFrame(idr) || isRestartFrame(delta(1)) {
		t.Fatal("restart classification wrong")
	}

	p.EnqueueBinaryFrame(key)
	p.EnqueueBinaryFrame(delta(1))
	p.EnqueueBinaryFrame(delta(2))
	if len(p.frameCh) != 3 {
		t.Fatalf("deltas must queue in order behind the key, got %d", len(p.frameCh))
	}
	p.EnqueueBinaryFrame(key) // newer key makes the queued chain obsolete
	if len(p.frameCh) != 1 {
		t.Fatalf("key frame should replace the queue, got %d", len(p.frameCh))
	}
	for i := 0; i < frameQueueLen; i++ {
		p.EnqueueBinaryFrame(delta(byte(i)))
	}
	if len(p.frameCh) != 0 {
		t.Fatalf("overflowing delta must flush the whole chain, got %d", len(p.frameCh))
	}
}

func TestMetricsRequireToken(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	t.Setenv("OPENDESK_METRICS_TOKEN", strings.Repeat("m", 32))
	for _, auth := range []string{"", "Bearer wrong"} {
		rec := httptest.NewRecorder()
		req := httptest.NewRequest("GET", "/metrics", nil)
		req.Header.Set("Authorization", auth)
		g.ServeHTTP(rec, req)
		if rec.Code != 401 {
			t.Fatalf("metrics exposed with %q: %d", auth, rec.Code)
		}
	}
	rec := httptest.NewRecorder()
	req := httptest.NewRequest("GET", "/metrics", nil)
	req.Header.Set("Authorization", "Bearer "+strings.Repeat("m", 32))
	g.ServeHTTP(rec, req)
	if rec.Code != 200 || !strings.Contains(rec.Body.String(), `"ingress"`) {
		t.Fatalf("metrics unavailable: %d %s", rec.Code, rec.Body.String())
	}
}

func TestSanitizeCaps(t *testing.T) {
	got := sanitizeCaps([]interface{}{"file-ack", "BAD", 7, strings.Repeat("x", 33), "p2p.v1"})
	if len(got) != 2 || got[0] != "file-ack" || got[1] != "p2p.v1" {
		t.Fatalf("unexpected caps: %v", got)
	}
}

func TestSessionAuthorization(t *testing.T) {
	g := NewGateway(4096, 4096, nil)
	a := &Peer{CleanID: "111"}
	b := &Peer{CleanID: "222"}
	other := &Peer{CleanID: "333"}
	g.peers[a.CleanID], g.peers[b.CleanID], g.peers[other.CleanID] = a, b, other

	if g.sessionPeer(a, b.CleanID) != nil || !g.setSession(a, b, false) {
		t.Fatal("unexpected session setup")
	}
	if g.sessionPeer(a, b.CleanID) != nil || g.sessionPeer(other, b.CleanID) != nil {
		t.Fatal("pending session accepted traffic")
	}
	if g.approveSession(other, b) || !g.approveSession(a, b) {
		t.Fatal("approval accepted wrong partner or rejected right partner")
	}
	if g.sessionPeer(a, b.CleanID) != b || g.sessionPeer(b, a.CleanID) != a || g.sessionPeer(a, other.CleanID) != nil {
		t.Fatal("traffic allowed outside active pair")
	}
	if g.closeSession(other, b.CleanID) != nil || g.closeSession(a, b.CleanID) != b || g.sessionPeer(b, a.CleanID) != nil {
		t.Fatal("unauthorized disconnect or stale session")
	}
	if !g.setSession(a, b, true) {
		t.Fatal("reconnect failed")
	}
	g.peers[a.CleanID] = &Peer{CleanID: a.CleanID}
	if g.sessionPeer(a, b.CleanID) != nil {
		t.Fatal("replaced connection retained access")
	}
}
