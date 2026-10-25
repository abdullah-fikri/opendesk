package usecase

import (
	"testing"

	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/model"
)

func multiFixture(n int) (*Relay, *Peer, []*Peer) {
	r := newTestRelay(Options{})
	v := addPeer(r, "100000000", true)
	hosts := make([]*Peer, n)
	for i := range hosts {
		hosts[i] = addPeer(r, "20000000"+string(rune('1'+i)), false)
	}
	return r, v, hosts
}

// viewOf returns the receiver's per-session queue for messages from sender, as ForwardBinary would use it.
func viewOf(r *Relay, sender *Peer) *viewSession {
	r.mu.RLock()
	defer r.mu.RUnlock()
	rcpt := r.activePartnerLocked(sender, "")
	if rcpt == nil {
		return nil
	}
	return rcpt.views[sender.CleanID]
}

func TestMultiSessionCapAndRouting(t *testing.T) {
	r, v, h := multiFixture(6)
	for i := 0; i < model.MaxViewSessions; i++ {
		if !r.openSession(v, h[i], true) {
			t.Fatalf("session %d rejected", i)
		}
	}
	if r.openSession(v, h[4], true) {
		t.Fatal("fifth concurrent session accepted")
	}
	if r.openSession(v, h[0], true) {
		t.Fatal("duplicate session to the same host accepted")
	}
	for i := 0; i < model.MaxViewSessions; i++ {
		if r.ActivePartner(v, h[i].CleanID) != h[i] || r.ActivePartner(h[i], "") != v || r.ActivePartner(h[i], v.CleanID) != v {
			t.Fatalf("session %d does not route both ways", i)
		}
		if viewOf(r, h[i]) == nil {
			t.Fatalf("host %d frames are not routed through the viewer's session queue", i)
		}
	}
	if r.ActivePartner(v, "") != nil || r.ActivePartner(v, h[4].CleanID) != nil {
		t.Fatal("multi-session viewer routed without naming a session it holds")
	}
	// A busy multi-session viewer cannot be taken as host, and its hosts stay single-session.
	single := addPeer(r, "300000000", false)
	if r.openSession(single, v, true) || r.openSession(single, h[0], true) {
		t.Fatal("busy viewer or host accepted a second controller")
	}
	if r.openSession(h[5], v, true) {
		t.Fatal("viewer with sessions became a host")
	}
}

func TestMultiSessionIsolation(t *testing.T) {
	r, v, h := multiFixture(5)
	for i := 0; i < 4; i++ {
		r.openSession(v, h[i], true)
	}
	// Viewer closes one tab: only that pair ends.
	if r.Disconnect(v, h[1].CleanID) != h[1] || h[1].sessionWith != "" || r.ActivePartner(v, h[1].CleanID) != nil {
		t.Fatal("closing a tab did not end its session")
	}
	for _, i := range []int{0, 2, 3} {
		if r.ActivePartner(v, h[i].CleanID) != h[i] {
			t.Fatalf("closing one tab tore down session %d", i)
		}
	}
	// Host ends its side: the viewer's other sessions survive.
	if r.Disconnect(h[2], v.CleanID) != v || v.views[h[2].CleanID] != nil || r.ActivePartner(v, h[0].CleanID) != h[0] {
		t.Fatal("host-side disconnect wrong")
	}
	// A freed slot is reusable; pending sessions (Android approval) are per pair.
	if !r.openSession(v, h[4], false) || r.ActivePartner(v, h[4].CleanID) != nil {
		t.Fatal("pending session setup wrong")
	}
	if r.Approve(v, h[0]) == nil || r.Approve(v, h[4]) != nil || r.ActivePartner(h[4], v.CleanID) != v {
		t.Fatal("approval not scoped to the pending pair")
	}
	// Host connection drops: viewer is told about that host only.
	removed, partners := r.Unregister(h[0])
	if !removed || len(partners) != 1 || partners[0] != v || v.views[h[0].CleanID] != nil || r.ActivePartner(v, h[3].CleanID) != h[3] {
		t.Fatalf("host removal leaked into other sessions: %v", partners)
	}
	// Viewer connection drops: every remaining host is released and notified.
	removed, partners = r.Unregister(v)
	if !removed || len(partners) != 2 || h[3].sessionWith != "" || h[4].sessionWith != "" || len(v.views) != 0 {
		t.Fatalf("viewer removal did not release all hosts: %d", len(partners))
	}
}

func TestMultiSessionFileScoping(t *testing.T) {
	r, v, h := multiFixture(3)
	r.openSession(v, h[0], true)
	r.openSession(v, h[1], true)
	if r.RouteFile(v, h[0].CleanID, FileStart, "ft_1") != h[0] {
		t.Fatal("transfer rejected inside session")
	}
	if r.RouteFile(v, h[1].CleanID, FileChunk, "ft_1") != nil || r.RouteFile(v, "", FileChunk, "ft_1") != h[0] {
		t.Fatal("grant not bound to its pair")
	}
	if r.RouteFile(h[1], v.CleanID, FileAck, "ft_1") != nil || r.RouteFile(h[0], v.CleanID, FileAck, "ft_1") != v {
		t.Fatal("ack routed across sessions")
	}
	if r.RouteFile(v, h[1].CleanID, FileOffer, "of_1") != h[1] || r.RouteFile(h[1], v.CleanID, FilePull, "of_1") != v ||
		r.RouteFile(h[2], v.CleanID, FilePull, "of_1") != nil {
		t.Fatal("offer/pull not scoped to the session")
	}
	if r.RouteFile(v, h[2].CleanID, FileStart, "ft_2") != nil {
		t.Fatal("transfer to a host without a session")
	}
}

func TestSingleSessionViewerStaysSingle(t *testing.T) {
	r := newTestRelay(Options{})
	a, b, c := addPeer(r, "100000000", false), addPeer(r, "200000001", false), addPeer(r, "200000002", false)
	if !r.openSession(a, b, true) || r.openSession(a, c, true) || a.views != nil {
		t.Fatal("single-session viewer got a second session")
	}
	if !r.ForwardBinary(b, []byte{0xFF, 0xD8, 1}) || len(a.frames.ch) != 1 {
		t.Fatal("single-session viewer frames must use the peer queue, untagged")
	}
	// A single-session sender's 0x10 binary is not parsed as a session tag (the tag names no session).
	tagged := append([]byte{sessionTag}, []byte("999999999")...)
	if !r.ForwardBinary(a, append(tagged, 0x02, 48)) {
		t.Fatal("tag parsed for a single-session peer")
	}
}

func TestViewQueuesAreIndependent(t *testing.T) {
	m := metrics.New()
	one, two := newFrameQueue(m), newFrameQueue(m)
	key := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0}
	delta := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 2, 0, 1}
	one.push(key)
	one.push(delta)
	two.push(key)
	two.push(delta)
	two.push(key) // restart on session two only
	if len(one.ch) != 2 || len(two.ch) != 1 {
		t.Fatalf("a restart frame leaked across sessions: %d %d", len(one.ch), len(two.ch))
	}
}
