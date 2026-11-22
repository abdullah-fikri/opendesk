package usecase

import (
	"fmt"
	"slices"
	"sync"
	"testing"
	"time"
)

// lanePeer registers a peer that listed file-lane and file-binary, without a main writer (see addPeer).
func lanePeer(r *Relay, id string) *Peer {
	p := addPeer(r, id, false)
	p.FileLane, p.FileBinary = true, true
	return p
}

// attach issues a token to p and attaches a lane on conn with it.
func attach(t *testing.T, r *Relay, p *Peer, conn Conn) *Lane {
	t.Helper()
	token, err := r.IssueLaneToken(p)
	if err != nil {
		t.Fatal(err)
	}
	l, _, err := r.AttachLane(conn, p.CleanID, token)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(l.Close)
	return l
}

// tagged is a binary chunk as a lane carries it: the receiver's tag, then the chunk.
func tagged(to, id string, data string) []byte {
	return append([]byte("\x10"+to), fileChunk(id, 0, data)...)
}

// Only the device's own current main connection gets a token, only it can use it, once and only soon.
func TestLaneAttachNeedsFreshTokenOfTheDevice(t *testing.T) {
	r := newTestRelay(Options{})
	a, b := lanePeer(r, "111111111"), lanePeer(r, "222222222")
	legacy := addPeer(r, "333333333", false)
	if _, err := r.IssueLaneToken(legacy); err == nil {
		t.Fatal("token issued to a client without file-lane")
	}
	if _, _, err := r.AttachLane(nopConn{}, a.CleanID, ""); err == nil {
		t.Fatal("attached without a token")
	}
	token, err := r.IssueLaneToken(a)
	if err != nil || len(token) < 32 {
		t.Fatalf("token %q, %v", token, err)
	}
	for name, try := range map[string]struct{ id, token string }{
		"wrong token":         {a.CleanID, token[:len(token)-1] + "x"},
		"another device's id": {b.CleanID, token},
		"unknown device":      {"999999999", token},
	} {
		if _, _, err := r.AttachLane(nopConn{}, try.id, try.token); err == nil {
			t.Fatalf("%s accepted", name)
		}
	}
	l, _, err := r.AttachLane(nopConn{}, a.CleanID, token)
	if err != nil || l.Owner != a || a.lane != l {
		t.Fatalf("valid token refused: %v", err)
	}
	defer l.Close()
	if _, _, err := r.AttachLane(nopConn{}, a.CleanID, token); err == nil {
		t.Fatal("spent token accepted twice")
	}

	// Expired.
	token, _ = r.IssueLaneToken(a)
	a.laneTokenExpires = time.Now().Add(-time.Second)
	if _, _, err := r.AttachLane(nopConn{}, a.CleanID, token); err == nil {
		t.Fatal("expired token accepted")
	}
	// Issued to a registration that has since been replaced.
	token, _ = r.IssueLaneToken(a)
	replaced := lanePeer(r, a.CleanID)
	if _, _, err := r.AttachLane(nopConn{}, a.CleanID, token); err == nil {
		t.Fatal("token of a replaced registration accepted")
	}
	if _, err := r.IssueLaneToken(a); err == nil {
		t.Fatal("token issued to a replaced registration")
	}
	// A suspended registration (waiting for a resume) takes no lane.
	token, _ = r.IssueLaneToken(replaced)
	replaced.suspendedUntil = time.Now().Add(time.Minute)
	if _, _, err := r.AttachLane(nopConn{}, replaced.CleanID, token); err == nil {
		t.Fatal("suspended registration took a lane")
	}
	// A relay with lanes off issues nothing and does not advertise them.
	off := newTestRelay(Options{FileLaneDisabled: true})
	if _, err := off.IssueLaneToken(lanePeer(off, "444444444")); err == nil || slices.Contains(off.Capabilities(), "file-lane") {
		t.Fatal("lane offered by a relay that disabled it")
	}
	if !slices.Contains(r.Capabilities(), "file-lane") {
		t.Fatal("file-lane not advertised")
	}
}

// Lane traffic passes the main connection's rules (an active session to start, the grant afterwards), plus its
// own: only start, chunk and end; a transfer never changes connection; binary chunks name their receiver.
func TestLaneForwardingKeepsTheFileRules(t *testing.T) {
	r := newTestRelay(Options{})
	a, b, stranger := lanePeer(r, "111111111"), lanePeer(r, "222222222"), lanePeer(r, "333333333")
	laneConn := newGateConn(false)
	la, lb := attach(t, r, a, nopConn{}), attach(t, r, b, laneConn)

	if rcpt, _ := r.RouteLaneFile(la, b.CleanID, FileStart, "ft_1"); rcpt != nil {
		t.Fatal("transfer started without a session")
	}
	r.openSession(a, b, true)
	if rcpt, _ := r.RouteLaneFile(la, stranger.CleanID, FileStart, "ft_1"); rcpt != nil {
		t.Fatal("transfer started to a device outside the session")
	}
	if rcpt, _ := r.RouteLaneFile(lb, a.CleanID, FileOffer, "ft_1"); rcpt != nil {
		t.Fatal("offer accepted on a lane")
	}
	rcpt, out := r.RouteLaneFile(la, b.CleanID, FileStart, "ft_1")
	if rcpt != b || out != lb {
		t.Fatalf("lane transfer not delivered on the receiver's lane: %v %v", rcpt, out)
	}
	if r.ForwardBinary(a, fileChunk("ft_1", 0, "x")) || r.RouteFile(a, b.CleanID, FileEnd, "ft_1") != nil {
		t.Fatal("lane transfer continued on the main connection")
	}
	if r.ForwardLaneBinary(la, fileChunk("ft_1", 0, "x")) || r.ForwardLaneBinary(la, tagged(stranger.CleanID, "ft_1", "x")) {
		t.Fatal("untagged or misaddressed chunk forwarded")
	}
	if r.ForwardLaneBinary(lb, tagged(a.CleanID, "ft_1", "x")) {
		t.Fatal("grant used by its receiver")
	}
	if !r.ForwardLaneBinary(la, tagged(b.CleanID, "ft_1", "abc")) {
		t.Fatal("granted chunk rejected")
	}
	want := fmt.Sprintf("b:%q", "\x10"+a.CleanID+string(fileChunk("ft_1", 0, "abc")))
	if got := laneConn.waitEntries(t, 1); got[0] != want {
		t.Fatalf("lane wrote %v, want %s", got, want)
	}
	if b.file.n != 0 || b.control.n != 0 {
		t.Fatal("lane traffic queued on the receiver's main connection")
	}
	// A main-connection transfer never moves to a lane either.
	if r.RouteFile(a, b.CleanID, FileStart, "ft_2") != b {
		t.Fatal("main transfer rejected")
	}
	if rcpt, _ := r.RouteLaneFile(la, b.CleanID, FileChunk, "ft_2"); rcpt != nil {
		t.Fatal("main transfer continued on a lane")
	}
	// The receiver's verdict goes back on its main connection, as always.
	if r.RouteFile(b, a.CleanID, FileAck, "ft_1") != a {
		t.Fatal("ack of a lane transfer rejected")
	}
}

// Every transfer from one lane to one receiver uses the same receiver connection while any of them runs, so the
// receiver sees that lane's chunks in order; once they are done, a new one takes the receiver's lane.
func TestLaneTransfersToOneReceiverShareItsConnection(t *testing.T) {
	r := newTestRelay(Options{})
	a, b := lanePeer(r, "111111111"), lanePeer(r, "222222222")
	r.openSession(a, b, true)
	la := attach(t, r, a, nopConn{})
	if rcpt, out := r.RouteLaneFile(la, b.CleanID, FileStart, "ft_1"); rcpt != b || out != nil {
		t.Fatal("receiver without a lane must get lane transfers on its main connection")
	}
	lb := attach(t, r, b, nopConn{})
	if _, out := r.RouteLaneFile(la, b.CleanID, FileStart, "ft_2"); out != nil {
		t.Fatal("second transfer split from the first one's connection")
	}
	r.RouteFile(b, a.CleanID, FileAck, "ft_1")
	r.RouteFile(b, a.CleanID, FileAck, "ft_2")
	if _, out := r.RouteLaneFile(la, b.CleanID, FileStart, "ft_3"); out != lb {
		t.Fatal("new transfer did not take the receiver's lane")
	}
}

// A lane that drops cuts exactly its transfers, and the partner on the other end learns which; a lane ends with
// its main connection.
func TestLaneDropCutsItsTransfers(t *testing.T) {
	r := newTestRelay(Options{})
	a, b := lanePeer(r, "111111111"), lanePeer(r, "222222222")
	r.openSession(a, b, true)
	la := attach(t, r, a, nopConn{})
	r.RouteLaneFile(la, b.CleanID, FileStart, "ft_2")
	r.RouteLaneFile(la, b.CleanID, FileStart, "ft_1")
	r.RouteFile(a, b.CleanID, FileStart, "ft_main")
	lost := r.DetachLane(la)
	if len(lost) != 1 || lost[0].To != b || lost[0].From != a.CleanID || !slices.Equal(lost[0].IDs, []string{"ft_1", "ft_2"}) {
		t.Fatalf("lost %+v", lost)
	}
	if a.lane != nil || r.DetachLane(la) != nil {
		t.Fatal("lane still attached")
	}
	if rcpt, _ := r.RouteLaneFile(la, b.CleanID, FileChunk, "ft_1"); rcpt != nil {
		t.Fatal("detached lane still forwards")
	}
	if r.RouteFile(a, b.CleanID, FileChunk, "ft_main") != b {
		t.Fatal("main-connection transfer cut by the lane drop")
	}

	// The receiving lane drops: the sender learns.
	la = attach(t, r, a, nopConn{})
	lb := attach(t, r, b, nopConn{})
	r.RouteLaneFile(la, b.CleanID, FileStart, "ft_3")
	if lost := r.DetachLane(lb); len(lost) != 1 || lost[0].To != a || lost[0].From != b.CleanID || lost[0].IDs[0] != "ft_3" {
		t.Fatalf("lost %+v", lost)
	}

	// A new lane replaces the old one; the main connection leaving ends the lane.
	r.RouteLaneFile(la, b.CleanID, FileStart, "ft_4")
	la2 := attach(t, r, a, nopConn{})
	if a.lane != la2 {
		t.Fatal("new lane not attached")
	}
	r.RouteLaneFile(la2, b.CleanID, FileStart, "ft_5")
	res := r.Leave(a)
	if a.lane != nil || len(res.LaneLost) != 1 || res.LaneLost[0].IDs[0] != "ft_5" {
		t.Fatalf("leave: lane %v, lost %+v", a.lane, res.LaneLost)
	}
}

// One slow receiver costs the relay at most its lane queue: a full queue holds the sender back instead of
// growing, and everything arrives in order once the receiver reads again.
func TestLaneBackpressureBoundsMemory(t *testing.T) {
	r := newTestRelay(Options{})
	a, b := lanePeer(r, "111111111"), lanePeer(r, "222222222")
	r.openSession(a, b, true)
	stalled := newCountConn()
	la, lb := attach(t, r, a, nopConn{}), attach(t, r, b, stalled)
	r.RouteLaneFile(la, b.CleanID, FileStart, "ft_1")

	const chunks = 24
	data := string(make([]byte, 1<<20))
	done := make(chan int)
	go func() {
		n := 0
		for range chunks {
			if r.ForwardLaneBinary(la, tagged(b.CleanID, "ft_1", data)) {
				n++
			}
		}
		done <- n
	}()
	deadline := time.Now().Add(10 * time.Second)
	for {
		lb.out.outMu.Lock()
		queued, waiting := lb.out.file.bytes, lb.out.roomWaiters
		lb.out.outMu.Unlock()
		if queued > laneQueueBytes {
			t.Fatalf("lane queue grew to %d bytes", queued)
		}
		if waiting > 0 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("sender never waited for room (%d bytes queued)", queued)
		}
		time.Sleep(time.Millisecond)
	}
	select {
	case <-done:
		t.Fatal("sender went on past a full lane")
	default:
	}
	close(stalled.gate)
	if n := <-done; n != chunks {
		t.Fatalf("%d of %d chunks forwarded", n, chunks)
	}
	deadline = time.Now().Add(10 * time.Second)
	for stalled.count() < chunks {
		if time.Now().After(deadline) {
			t.Fatalf("%d of %d chunks written", stalled.count(), chunks)
		}
		time.Sleep(time.Millisecond)
	}
}

// countConn counts writes without keeping them; every write waits until gate is closed (a receiver whose TCP
// window is full), so large payloads cost the test nothing to record.
type countConn struct {
	gate chan struct{}
	mu   sync.Mutex
	n    int
}

func newCountConn() *countConn { return &countConn{gate: make(chan struct{})} }

func (c *countConn) write() error {
	<-c.gate
	c.mu.Lock()
	c.n++
	c.mu.Unlock()
	return nil
}

func (c *countConn) count() int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.n
}

func (c *countConn) WriteText([]byte) error      { return c.write() }
func (c *countConn) WriteBinary(...[]byte) error { return c.write() }
func (c *countConn) Ping() error                 { return nil }
func (c *countConn) Close() error                { return nil }
