package usecase

import (
	"bytes"
	"strings"
	"sync"
	"testing"
	"time"

	"opendesk/server-relay/internal/model"
)

// recConn records binary writes so a tile writer's output can be checked.
type recConn struct {
	nopConn
	mu  sync.Mutex
	bin [][]byte
}

func (c *recConn) WriteBinary(parts ...[]byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.bin = append(c.bin, bytes.Join(parts, nil))
	return nil
}

func (c *recConn) waitBinary(t *testing.T, n int) [][]byte {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		c.mu.Lock()
		if len(c.bin) >= n {
			out := c.bin
			c.mu.Unlock()
			return out
		}
		c.mu.Unlock()
		time.Sleep(time.Millisecond)
	}
	t.Fatalf("expected %d binary writes", n)
	return nil
}

// blockConn blocks every binary write until release is closed: an administrator on a stalled link.
type blockConn struct {
	nopConn
	release chan struct{}
}

func (c blockConn) WriteBinary(...[]byte) error {
	<-c.release
	return nil
}

func addAdmin(r *Relay, id string, conn Conn) *Peer {
	p := addPeer(r, id, true)
	p.IsAdmin, p.MonitorView, p.conn = true, true, conn
	return p
}

func addMonitorHost(r *Relay, id string) *Peer {
	p := addPeer(r, id, true)
	p.MonitorHost = true
	return p
}

var thumb = []byte{0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3}

func states(up MonitorUpdate) map[string]model.MonitorState {
	out := map[string]model.MonitorState{}
	for _, n := range up.Notices {
		out[n.Host] = n.State
	}
	return out
}

func TestMonitorRequiresAdministrator(t *testing.T) {
	r := newTestRelay(Options{})
	h := addMonitorHost(r, "200000001")
	viewer := addPeer(r, "100000001", true)
	viewer.MonitorView = true // the admin edition's capability without the admin role
	up, err := r.WatchMonitors(viewer, []string{h.CleanID})
	if err != ErrNotAdmin || len(up.Start) != 0 || states(up)[h.CleanID] != model.MonitorDenied || r.WatchCount(viewer) != 0 {
		t.Fatalf("non-administrator watched a host: %v %+v", err, up)
	}
	legacyAdmin := addPeer(r, "100000002", true)
	legacyAdmin.IsAdmin = true // an administrator without monitor-view cannot parse the stream
	if up, err := r.WatchMonitors(legacyAdmin, []string{h.CleanID}); err != ErrNotAdmin || len(up.Notices) != 0 {
		t.Fatalf("admin without monitor-view watched: %v %+v", err, up)
	}

	// With credentials configured, a claimed admin role is ignored: the same rule as get-hosts.
	token := strings.Repeat("t", 32)
	r = newTestRelay(Options{Credentials: map[string]model.DeviceCredential{
		"100000003": {Token: token},
		"100000004": {Token: token, Admin: true},
	}})
	caps := []string{model.CapMonitorView}
	claimed, _, err := r.Register(nopConn{}, PeerInfo{CleanID: "100000003", IsAdmin: true, Caps: caps}, token)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := r.WatchMonitors(claimed, []string{"200000001"}); err != ErrNotAdmin {
		t.Fatal("unprovisioned device claiming admin watched a host")
	}
	admin, _, _ := r.Register(nopConn{}, PeerInfo{CleanID: "100000004", Caps: caps}, token)
	if _, err := r.WatchMonitors(admin, []string{"200000001"}); err != nil || r.WatchCount(admin) != 1 {
		t.Fatalf("provisioned administrator refused: %v", err)
	}
	r.MonitorDetach(admin)
}

func TestMonitorIsReadOnly(t *testing.T) {
	r := newTestRelay(Options{})
	conn := &recConn{}
	admin := addAdmin(r, "100000001", conn)
	h := addMonitorHost(r, "200000001")
	up, err := r.WatchMonitors(admin, []string{h.CleanID})
	if err != nil || len(up.Start) != 1 || up.Start[0] != h || states(up)[h.CleanID] != model.MonitorLive {
		t.Fatalf("watch not started: %v %+v", err, up)
	}
	// Nothing session-scoped is authorized between the administrator and the watched host, either way.
	if r.ActivePartner(admin, h.CleanID) != nil || r.ActivePartner(h, admin.CleanID) != nil || r.ActivePartner(h, "") != nil {
		t.Fatal("a watch authorized input, clipboard or P2P signals")
	}
	if r.RouteFile(admin, h.CleanID, FileStart, "ft_1") != nil || r.RouteFile(admin, h.CleanID, FileOffer, "of_1") != nil ||
		r.RouteFile(h, admin.CleanID, FileStart, "ft_2") != nil {
		t.Fatal("a watch authorized a file transfer")
	}
	tagged := append([]byte{sessionTag}, h.CleanID...)
	for _, msg := range [][]byte{
		append(tagged, 0x02, 48, 1), append(tagged, thumb...), {0x02, 48, 1}, thumb,
		append([]byte{monitorTag}, thumb...), // an administrator cannot inject thumbnails
	} {
		if r.ForwardBinary(admin, msg) {
			t.Fatalf("administrator binary % x reached the host", msg[:2])
		}
	}
	if r.ForwardBinary(h, []byte{0x02, 48, 1}) || r.ForwardBinary(h, thumb) {
		t.Fatal("watched host sent voice or session video to its watcher")
	}
	// Only standalone JPEG thumbnails are fanned out; framed (delta) video is refused.
	delta := []byte{monitorTag, 0xFF, 0xD8, 0x00, 0x4F, 1, 2, 0, 1}
	if r.ForwardBinary(h, delta) || r.ForwardBinary(h, append([]byte{monitorTag}, make([]byte, maxMonitorFrameBytes+1)...)) {
		t.Fatal("framed or oversized thumbnail accepted")
	}
	if !r.ForwardBinary(h, append([]byte{monitorTag}, thumb...)) {
		t.Fatal("thumbnail refused")
	}
	got := conn.waitBinary(t, 1)[0]
	if !bytes.Equal(got, append(append([]byte{monitorTag}, h.CleanID...), thumb...)) {
		t.Fatalf("thumbnail not tagged with its host: % x", got)
	}
	// A host that was not asked to stream (nobody watches it) is refused too.
	other := addMonitorHost(r, "200000002")
	if r.ForwardBinary(other, append([]byte{monitorTag}, thumb...)) {
		t.Fatal("unwatched host fanned out a thumbnail")
	}
	r.MonitorDetach(admin)
}

func TestMonitorLeavesSessionsAlone(t *testing.T) {
	r := newTestRelay(Options{})
	admin := addAdmin(r, "100000001", nopConn{})
	hosts := make([]string, 0, model.MaxMonitorTiles+2)
	for i := range model.MaxMonitorTiles + 1 {
		hosts = append(hosts, addMonitorHost(r, "2000000"+string(rune('1'+i/10))+string(rune('0'+i%10))).CleanID)
	}
	hosts = append(hosts, hosts[0], "012345678", admin.CleanID) // duplicate, invalid, self
	up, _ := r.WatchMonitors(admin, hosts)
	if r.WatchCount(admin) != model.MaxMonitorTiles || len(up.Start) != model.MaxMonitorTiles {
		t.Fatalf("watch cap: %d tiles, %d starts", r.WatchCount(admin), len(up.Start))
	}
	// A watched host still accepts an interactive session, and the watcher keeps all four interactive slots.
	viewer := addPeer(r, "300000001", true)
	if !r.openSession(viewer, r.Lookup(hosts[0]), true) || r.ActivePartner(viewer, hosts[0]) == nil {
		t.Fatal("watched host refused an interactive session")
	}
	for i := 1; i <= model.MaxViewSessions; i++ {
		if !r.openSession(admin, r.Lookup(hosts[i]), true) {
			t.Fatalf("watching used up interactive session %d", i)
		}
	}
	// The session's own video still flows, untouched by the thumbnail stream next to it.
	h0 := r.Lookup(hosts[0])
	if !r.ForwardBinary(h0, []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0}) || !r.ForwardBinary(h0, append([]byte{monitorTag}, thumb...)) {
		t.Fatal("session video and thumbnails do not coexist")
	}
	r.MonitorDetach(admin)
}

func TestMonitorLifecycle(t *testing.T) {
	r := newTestRelay(Options{})
	bConn := &recConn{}
	a, b := addAdmin(r, "100000001", nopConn{}), addAdmin(r, "100000002", bConn)
	h := addMonitorHost(r, "200000001")
	legacy := addPeer(r, "200000002", false)
	const offline = "200000003"

	up, _ := r.WatchMonitors(a, []string{h.CleanID, legacy.CleanID, offline})
	st := states(up)
	if st[h.CleanID] != model.MonitorLive || st[legacy.CleanID] != model.MonitorUnsupported || st[offline] != model.MonitorOffline || len(up.Start) != 1 {
		t.Fatalf("initial states: %+v", up)
	}
	// A second watcher shares the stream: no second monitor-start, and it sees the newest frame at once.
	r.ForwardBinary(h, append([]byte{monitorTag}, thumb...))
	up, _ = r.WatchMonitors(b, []string{h.CleanID})
	if len(up.Start) != 0 {
		t.Fatalf("shared feed restarted: %+v", up)
	}
	bConn.waitBinary(t, 1) // the late joiner got the newest frame without waiting for the next one
	// Re-sending the same set changes nothing.
	if up, _ = r.WatchMonitors(a, []string{h.CleanID, legacy.CleanID, offline}); len(up.Notices)+len(up.Start)+len(up.Stop) != 0 {
		t.Fatalf("unchanged set produced %+v", up)
	}
	// The host drops: both watchers see it offline, the watches stay and go live again on reconnect.
	up = r.MonitorDetach(h)
	if len(up.Notices) != 2 || up.Notices[0].State != model.MonitorOffline {
		t.Fatalf("host drop: %+v", up)
	}
	r.ForwardBinary(h, append([]byte{monitorTag}, thumb...))
	h2 := addMonitorHost(r, h.CleanID) // the same desk ID on a new connection
	up = r.MonitorAttach(h2)
	if len(up.Start) != 1 || up.Start[0] != h2 || len(up.Notices) != 2 || up.Notices[0].State != model.MonitorLive {
		t.Fatalf("reconnect: %+v", up)
	}
	if up = r.MonitorAttach(h2); len(up.Start) != 0 {
		t.Fatal("attach twice started twice")
	}
	if r.ForwardBinary(h, append([]byte{monitorTag}, thumb...)) {
		t.Fatal("replaced connection still streams")
	}
	// An offline host coming online starts streaming for its watcher.
	if up = r.MonitorAttach(addMonitorHost(r, offline)); len(up.Start) != 1 || up.Notices[0].Admin != a {
		t.Fatalf("offline host going online: %+v", up)
	}
	// Page switch: a stops watching everything; the shared host keeps streaming for b, the others stop.
	up, _ = r.WatchMonitors(a, nil)
	if len(up.Stop) != 1 || up.Stop[0].CleanID != offline || r.WatchCount(a) != 0 {
		t.Fatalf("unwatch: %+v", up)
	}
	// The last watcher leaving stops the host.
	if up = r.MonitorDetach(b); len(up.Stop) != 1 || up.Stop[0] != h2 || len(r.monitors) != 0 {
		t.Fatalf("last watcher left: %+v, %d feeds", up, len(r.monitors))
	}
}

func TestMonitorSlowAdministratorDropsOldFrames(t *testing.T) {
	r := newTestRelay(Options{})
	stalled := blockConn{release: make(chan struct{})}
	admin := addAdmin(r, "100000001", stalled)
	h := addMonitorHost(r, "200000001")
	r.WatchMonitors(admin, []string{h.CleanID})
	frame := append([]byte{monitorTag}, thumb...)
	for range 200 {
		r.ForwardBinary(h, frame)
	}
	// One frame in the stalled writer plus at most one waiting: old thumbnails are dropped, never queued.
	if n := len(admin.watching[h.CleanID].frames.ch); n > 1 {
		t.Fatalf("stalled tile queued %d frames", n)
	}
	close(stalled.release)
	r.MonitorDetach(admin)
}

// One host's thumbnail fanned out to every administrator watching it (16 administrators, 30 KiB frames).
func BenchmarkMonitorFanOut(b *testing.B) {
	r := newTestRelay(Options{})
	h := addMonitorHost(r, "200000001")
	for i := range 16 {
		admin := addAdmin(r, "1000000"+string(rune('1'+i/10))+string(rune('0'+i%10)), nopConn{})
		r.WatchMonitors(admin, []string{h.CleanID})
	}
	frame := append([]byte{monitorTag, 0xFF, 0xD8, 0xFF}, make([]byte, 30<<10)...)
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		if !r.ForwardBinary(h, frame) {
			b.Fatal("fan-out refused")
		}
	}
}

// One administrator's full 4x4 page: 16 hosts each pushing a thumbnail.
func BenchmarkMonitorPage(b *testing.B) {
	r := newTestRelay(Options{})
	admin := addAdmin(r, "100000001", nopConn{})
	hosts := make([]*Peer, 16)
	ids := make([]string, 16)
	for i := range hosts {
		hosts[i] = addMonitorHost(r, "2000000"+string(rune('1'+i/10))+string(rune('0'+i%10)))
		ids[i] = hosts[i].CleanID
	}
	r.WatchMonitors(admin, ids)
	frame := append([]byte{monitorTag, 0xFF, 0xD8, 0xFF}, make([]byte, 30<<10)...)
	b.ReportAllocs()
	b.ResetTimer()
	for b.Loop() {
		for _, h := range hosts {
			r.ForwardBinary(h, frame)
		}
	}
}
