package usecase

import (
	"context"
	"log/slog"
	"testing"
	"time"

	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/model"
)

// nopStore satisfies both repositories without storing anything.
type nopStore struct{}

func (nopStore) Upsert(context.Context, model.Host) error                                { return nil }
func (nopStore) MarkOffline(context.Context, string, time.Time) error                    { return nil }
func (nopStore) MarkAllOffline(context.Context, time.Time) error                         { return nil }
func (nopStore) List(context.Context) ([]model.Host, error)                              { return nil, nil }
func (nopStore) Start(context.Context, model.SessionRecord, bool) error                  { return nil }
func (nopStore) UpdateStatus(context.Context, string, string, model.SessionStatus) error { return nil }
func (nopStore) CloseForPeer(context.Context, string) error                              { return nil }
func (nopStore) CloseAllOpen(context.Context) error                                      { return nil }

// nopConn discards writes.
type nopConn struct{}

func (nopConn) WriteText([]byte) error      { return nil }
func (nopConn) WriteBinary(...[]byte) error { return nil }
func (nopConn) Ping() error                 { return nil }
func (nopConn) Close() error                { return nil }

func newTestRelay(opts Options) *Relay {
	return New(nopStore{}, nopStore{}, metrics.New(), opts, slog.New(slog.DiscardHandler))
}

// addPeer registers a peer directly, without frame writer goroutines, so tests can inspect its queues.
func addPeer(r *Relay, id string, multi bool) *Peer {
	p := &Peer{CleanID: id, Multi: multi, conn: nopConn{}, frames: newFrameQueue(r.metrics), closeCh: make(chan struct{}), metrics: r.metrics}
	r.peers[id] = p
	return p
}

func BenchmarkActivePartner(b *testing.B) {
	r := newTestRelay(Options{})
	a, peer := addPeer(r, "111111111", false), addPeer(r, "222222222", false)
	if !r.openSession(a, peer, true) {
		b.Fatal("session setup failed")
	}
	b.ReportAllocs()
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		if r.ActivePartner(a, peer.CleanID) != peer {
			b.Fatal("routing failed")
		}
	}
}

func TestDeviceAuthentication(t *testing.T) {
	r := newTestRelay(Options{})
	if ok, admin := r.authenticate("123456789", "", true); !ok || !admin {
		t.Fatal("open relay (no credentials configured) must admit devices with their claimed role")
	}
	token := "0123456789abcdef0123456789abcdef"
	r = newTestRelay(Options{Credentials: map[string]model.DeviceCredential{"123456789": {Token: token, Admin: true}}})
	for _, in := range []struct{ id, token string }{{"123456789", ""}, {"123456789", "wrong"}, {"987654321", token}} {
		if ok, _ := r.authenticate(in.id, in.token, true); ok {
			t.Fatalf("accepted invalid credential for %s", in.id)
		}
	}
	if ok, admin := r.authenticate("123456789", token, false); !ok || !admin {
		t.Fatal("rejected provisioned administrator")
	}
}

func TestRegisterValidatesAndReplaces(t *testing.T) {
	r := newTestRelay(Options{})
	if _, _, err := r.Register(nopConn{}, PeerInfo{CleanID: "012345678"}, ""); err != ErrInvalidDeskID {
		t.Fatalf("leading zero accepted: %v", err)
	}
	first, _, err := r.Register(nopConn{}, PeerInfo{CleanID: "111111111", Caps: []string{"multi-session", "BAD"}}, "")
	if err != nil || !first.Multi || len(first.Caps) != 1 || first.DeskID != "111 111 111" {
		t.Fatalf("registration wrong: %+v %v", first, err)
	}
	host, _, _ := r.Register(nopConn{}, PeerInfo{CleanID: "222222222"}, "")
	if _, err := r.Connect(first, host.CleanID); err != nil {
		t.Fatal(err)
	}
	second, orphans, _ := r.Register(nopConn{}, PeerInfo{CleanID: "111111111"}, "")
	if r.IsCurrent(first) || !r.IsCurrent(second) || len(orphans) != 1 || orphans[0] != host {
		t.Fatal("reconnect did not replace the old connection and release its partner")
	}
}

func TestFileGrantOutlivesSessionOnly(t *testing.T) {
	r := newTestRelay(Options{})
	a, b, other := addPeer(r, "111", false), addPeer(r, "222", false), addPeer(r, "333", false)

	if r.RouteFile(a, b.CleanID, FileStart, "ft_1") != nil {
		t.Fatal("transfer started without session")
	}
	if !r.openSession(a, b, true) || r.RouteFile(a, b.CleanID, FileStart, "ft_1") != b {
		t.Fatal("transfer rejected inside session")
	}
	if r.RouteFile(a, b.CleanID, FileStart, "ft_1") != nil {
		t.Fatal("duplicate transfer id accepted")
	}
	r.Disconnect(a, b.CleanID)
	if r.RouteFile(a, b.CleanID, FileChunk, "ft_1") != b || r.RouteFile(a, "", FileEnd, "ft_1") != b {
		t.Fatal("granted transfer stopped after session ended")
	}
	if r.RouteFile(other, b.CleanID, FileChunk, "ft_1") != nil || r.RouteFile(a, other.CleanID, FileChunk, "ft_1") != nil ||
		r.RouteFile(a, b.CleanID, FileChunk, "ft_2") != nil {
		t.Fatal("grant reused by another peer, target or transfer")
	}
	if r.RouteFile(other, a.CleanID, FileAck, "ft_1") != nil || r.RouteFile(b, a.CleanID, FileAck, "ft_1") != a {
		t.Fatal("ack routed to wrong peer")
	}
	if r.RouteFile(a, b.CleanID, FileChunk, "ft_1") != nil || r.RouteFile(b, a.CleanID, FileAck, "ft_1") != nil {
		t.Fatal("grant survived ack")
	}

	if !r.openSession(a, b, true) || r.RouteFile(a, b.CleanID, FileStart, "ft_3") != b {
		t.Fatal("second transfer rejected")
	}
	addPeer(r, b.CleanID, false) // receiver reconnected
	if r.RouteFile(a, b.CleanID, FileChunk, "ft_3") != nil {
		t.Fatal("grant followed a replaced connection")
	}
	r.grants[grantKey(a.CleanID, "ft_4")] = &fileGrant{from: a, to: r.peers[b.CleanID], expires: time.Now().Add(-time.Second)}
	if r.RouteFile(a, b.CleanID, FileChunk, "ft_4") != nil {
		t.Fatal("expired grant accepted")
	}
}

func TestClipboardOfferNeedsSession(t *testing.T) {
	r := newTestRelay(Options{})
	a, b, other := addPeer(r, "111", false), addPeer(r, "222", false), addPeer(r, "333", false)

	if r.RouteFile(a, b.CleanID, FileOffer, "of_1") != nil {
		t.Fatal("offer routed without session")
	}
	r.openSession(a, b, true)
	if r.RouteFile(a, b.CleanID, FileOffer, "of_1") != b || r.RouteFile(b, a.CleanID, FilePull, "of_1") != a {
		t.Fatal("offer/pull rejected inside session")
	}
	if r.RouteFile(other, a.CleanID, FilePull, "of_1") != nil {
		t.Fatal("pull accepted from outside the session")
	}
	r.Disconnect(a, b.CleanID)
	if r.RouteFile(b, a.CleanID, FilePull, "of_1") != nil {
		t.Fatal("pull outlived the session")
	}
}

func TestFrameQueueKeepsDeltaOrderAndRestartsCleanly(t *testing.T) {
	q := newFrameQueue(metrics.New())
	key := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0}
	delta := func(n byte) []byte { return []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 2, 0, n} }
	idr := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 3, 1}
	if !isRestartFrame([]byte{0xFF, 0xD8, 0xFF, 0xE0, 0, 0, 0}) || !isRestartFrame(key) || !isRestartFrame(idr) || isRestartFrame(delta(1)) {
		t.Fatal("restart classification wrong")
	}

	q.push(key)
	q.push(delta(1))
	q.push(delta(2))
	if len(q.ch) != 3 {
		t.Fatalf("deltas must queue in order behind the key, got %d", len(q.ch))
	}
	q.push(key) // a newer key makes the queued chain obsolete
	if len(q.ch) != 1 {
		t.Fatalf("key frame should replace the queue, got %d", len(q.ch))
	}
	for i := 0; i < frameQueueLen; i++ {
		q.push(delta(byte(i)))
	}
	if len(q.ch) != 0 {
		t.Fatalf("overflowing delta must flush the whole chain, got %d", len(q.ch))
	}
}

func TestSessionAuthorization(t *testing.T) {
	r := newTestRelay(Options{})
	a, b, other := addPeer(r, "111", false), addPeer(r, "222", false), addPeer(r, "333", false)

	if r.ActivePartner(a, b.CleanID) != nil || !r.openSession(a, b, false) {
		t.Fatal("unexpected session setup")
	}
	if r.ActivePartner(a, b.CleanID) != nil || r.ActivePartner(other, b.CleanID) != nil {
		t.Fatal("pending session accepted traffic")
	}
	if r.Approve(other, b) == nil || r.Approve(a, b) != nil {
		t.Fatal("approval accepted wrong partner or rejected right partner")
	}
	if r.ActivePartner(a, b.CleanID) != b || r.ActivePartner(b, a.CleanID) != a || r.ActivePartner(a, other.CleanID) != nil {
		t.Fatal("traffic allowed outside active pair")
	}
	if r.Disconnect(other, b.CleanID) != nil || r.Disconnect(a, b.CleanID) != b || r.ActivePartner(b, a.CleanID) != nil {
		t.Fatal("unauthorized disconnect or stale session")
	}
	if !r.openSession(a, b, true) {
		t.Fatal("reconnect failed")
	}
	addPeer(r, a.CleanID, false)
	if r.ActivePartner(a, b.CleanID) != nil {
		t.Fatal("replaced connection retained access")
	}
}

func TestRejectEndsPendingSessionOnly(t *testing.T) {
	r := newTestRelay(Options{})
	viewer, host := addPeer(r, "111", false), addPeer(r, "222", false)
	if _, err := r.Reject(host, viewer.CleanID); err == nil {
		t.Fatal("reject without a pending session")
	}
	r.openSession(viewer, host, false)
	if got, err := r.Reject(host, viewer.CleanID); err != nil || got != viewer || r.Approve(viewer, host) == nil {
		t.Fatal("reject did not end the pending session")
	}
	r.openSession(viewer, host, true)
	if _, err := r.Reject(host, viewer.CleanID); err == nil {
		t.Fatal("reject ended an active session")
	}
}

func TestViewerOnlyDevicesCannotBeControlled(t *testing.T) {
	r := newTestRelay(Options{})
	viewer := addPeer(r, "111111111", true)
	phone := addPeer(r, "222222222", false)
	phone.Caps = []string{model.CapViewerOnly}
	if _, err := r.Connect(viewer, phone.CleanID); err != ErrTargetViewerOnly {
		t.Fatalf("connect to a viewer-only device: got %v, want ErrTargetViewerOnly", err)
	}
	// The viewer-only device may still view others.
	host := addPeer(r, "333333333", false)
	if _, err := r.Connect(phone, host.CleanID); err != nil {
		t.Fatalf("viewer-only device could not view: %v", err)
	}
}
