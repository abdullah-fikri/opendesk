package usecase

import (
	"errors"
	"fmt"
	"sync"
	"testing"
	"time"

	"opendesk/server-relay/internal/metrics"
)

// gateConn records writes in order. While gate is open-ended (not closed) every write waits for it: a receiver
// whose TCP window is full. Close fails a waiting write, as closing a real socket does.
type gateConn struct {
	gate      chan struct{}
	closed    chan struct{}
	closeOnce sync.Once
	mu        sync.Mutex
	log       []string
}

func newGateConn(stalled bool) *gateConn {
	c := &gateConn{gate: make(chan struct{}), closed: make(chan struct{})}
	if !stalled {
		close(c.gate)
	}
	return c
}

func (c *gateConn) write(entry string) error {
	select {
	case <-c.gate:
	case <-c.closed:
		return errors.New("closed")
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	c.log = append(c.log, entry)
	return nil
}

func (c *gateConn) WriteText(data []byte) error { return c.write("t:" + string(data)) }

func (c *gateConn) WriteBinary(parts ...[]byte) error {
	var s string
	for _, p := range parts {
		s += string(p)
	}
	return c.write(fmt.Sprintf("b:%q", s))
}

func (c *gateConn) Ping() error  { return nil }
func (c *gateConn) Close() error { c.closeOnce.Do(func() { close(c.closed) }); return nil }

func (c *gateConn) isClosed() bool {
	select {
	case <-c.closed:
		return true
	default:
		return false
	}
}

func (c *gateConn) entries() []string {
	c.mu.Lock()
	defer c.mu.Unlock()
	return append([]string(nil), c.log...)
}

func (c *gateConn) waitEntries(t *testing.T, n int) []string {
	t.Helper()
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if e := c.entries(); len(e) >= n {
			return e
		}
		time.Sleep(time.Millisecond)
	}
	t.Fatalf("expected %d writes, got %v", n, c.entries())
	return nil
}

func waitWriterDone(t *testing.T, p *Peer) {
	t.Helper()
	select {
	case <-p.writerDone:
	case <-time.After(2 * time.Second):
		t.Fatal("writer goroutine did not exit")
	}
}

// A sender talking to a stalled receiver and a healthy one: the healthy receiver gets everything, voice to the
// stalled one drops its oldest packets, and the stalled one is closed when its control queue overflows.
func TestStalledReceiverNeverBlocksSender(t *testing.T) {
	m := metrics.New()
	stalledConn, healthyConn := newGateConn(true), newGateConn(false)
	stalled := newPeer(PeerInfo{CleanID: "200000001"}, "", false, stalledConn, m)
	healthy := newPeer(PeerInfo{CleanID: "200000002"}, "", false, healthyConn, m)

	started := time.Now()
	var err error
	for i := 0; i <= controlQueueLen+1 && err == nil; i++ {
		err = stalled.Send(map[string]int{"seq": i})
		if e := healthy.Send(map[string]int{"seq": i}); e != nil {
			t.Fatalf("healthy receiver refused input %d: %v", i, e)
		}
		stalled.deliverAudio(nil, "", []byte{audioMarker, 48, byte(i)})
	}
	// The only wait is the overflowing message's: up to controlStallLimit after the socket stopped taking data.
	if elapsed := time.Since(started); elapsed > controlStallLimit+300*time.Millisecond {
		t.Fatalf("sender blocked for %v on a stalled receiver", elapsed)
	}
	if !errors.Is(err, ErrSlowReceiver) {
		t.Fatalf("control overflow must close the receiver: err %v, closed %v", err, stalledConn.isClosed())
	}
	healthyConn.waitEntries(t, controlQueueLen+2)
	waitWriterDone(t, stalled)
	if err := stalled.Send("late"); !errors.Is(err, ErrPeerClosed) {
		t.Fatalf("closed peer accepted a message: %v", err)
	}
	snap := m.Snapshot(0)
	if snap["slowReceivers"].(uint64) != 1 || snap["droppedAudio"].(uint64) == 0 {
		t.Fatalf("overflow not counted: %v %v", snap["slowReceivers"], snap["droppedAudio"])
	}
	healthy.close()
	waitWriterDone(t, healthy)
}

// Classes leave in priority order (control, voice, video, file) and keep their own order.
func TestWriterPriorityAndOrder(t *testing.T) {
	conn := newGateConn(true)
	p := newPeer(PeerInfo{CleanID: "100000001"}, "", true, conn, metrics.New())
	defer p.close()
	_ = p.Send("first") // taken by the writer at once, which then waits on the gate
	time.Sleep(20 * time.Millisecond)
	view := p.startView("200000001", true)
	key := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0}
	_ = p.SendFile(FileChunk, "chunk1")
	_ = p.SendFile(FileChunk, "chunk2")
	p.deliverVideo(view, key)
	p.deliverAudio(view, "200000001", []byte{audioMarker, 48, 1})
	p.deliverAudio(nil, "", []byte{audioMarker, 48, 2})
	_ = p.Send("c1")
	_ = p.SendFile(FileOffer, "offer") // control class
	_ = p.Send("c2")
	close(conn.gate)

	got := conn.waitEntries(t, 9)
	want := []string{
		`t:"first"`, `t:"c1"`, `t:"offer"`, `t:"c2"`,
		fmt.Sprintf("b:%q", "\x10200000001\x02\x30\x01"), fmt.Sprintf("b:%q", "\x02\x30\x02"),
		fmt.Sprintf("b:%q", "\x10200000001"+string(key)),
		`t:"chunk1"`, `t:"chunk2"`,
	}
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Fatalf("write order\n got %v\nwant %v", got, want)
	}
}

// A full file queue holds its sender back until the receiver takes a message; video cannot starve it.
func TestFileBackpressure(t *testing.T) {
	conn := newGateConn(true)
	p := newPeer(PeerInfo{CleanID: "100000001"}, "", false, conn, metrics.New())
	_ = p.Send("hold") // the writer waits on the gate with this one
	time.Sleep(20 * time.Millisecond)
	for i := range fileQueueLen {
		if err := p.SendFile(FileChunk, i); err != nil {
			t.Fatal(err)
		}
	}
	sent := make(chan error, 1)
	go func() { sent <- p.SendFile(FileEnd, "end") }()
	select {
	case err := <-sent:
		t.Fatalf("file sender not held back by a full queue: %v", err)
	case <-time.After(100 * time.Millisecond):
	}
	// Video keeps arriving, yet the file queue drains and the sender continues.
	key := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0}
	stop := make(chan struct{})
	go func() {
		for {
			select {
			case <-stop:
				return
			default:
				p.deliverVideo(nil, key)
				time.Sleep(time.Millisecond)
			}
		}
	}()
	close(conn.gate)
	select {
	case err := <-sent:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("file sender still blocked after the receiver caught up")
	}
	close(stop)
	p.close()
	waitWriterDone(t, p)
}

// A burst far beyond the control queue to a healthy receiver holds the sender back briefly; nothing is lost.
func TestControlBurstToHealthyReceiver(t *testing.T) {
	conn := newGateConn(false)
	p := newPeer(PeerInfo{CleanID: "100000001"}, "", false, conn, metrics.New())
	const n = 20 * controlQueueLen
	for i := range n {
		if err := p.Send(i); err != nil {
			t.Fatalf("message %d refused: %v", i, err)
		}
	}
	got := conn.waitEntries(t, n)
	for i, e := range got {
		if e != fmt.Sprintf("t:%d", i) {
			t.Fatalf("message %d out of order: %s", i, e)
		}
	}
	p.close()
	waitWriterDone(t, p)
}

// Disconnect writes what was queued before it, then closes; a closed peer releases its writer and queues.
func TestDisconnectFlushesAndWriterExits(t *testing.T) {
	conn := newGateConn(false)
	p := newPeer(PeerInfo{CleanID: "100000001"}, "", true, conn, metrics.New())
	view := p.startView("200000001", true)
	_ = p.Send("bye")
	_ = p.Disconnect()
	waitWriterDone(t, p)
	if e := conn.entries(); len(e) != 1 || e[0] != `t:"bye"` || !conn.isClosed() {
		t.Fatalf("disconnect did not flush first: %v closed=%v", e, conn.isClosed())
	}
	view.frames.push([]byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0})
	p.removeStream(view.frames)
	if len(p.streams) != 0 || len(view.frames.ch) != 0 {
		t.Fatal("queues not released after the writer stopped")
	}

	// A writer blocked on a dead socket ends when the peer closes.
	stalledConn := newGateConn(true)
	q := newPeer(PeerInfo{CleanID: "100000002"}, "", false, stalledConn, metrics.New())
	_ = q.Send("stuck")
	time.Sleep(20 * time.Millisecond)
	q.kill()
	waitWriterDone(t, q)
}

// The hot text path to a receiver: one relayed mouse move, encoded and handed to the receiver's socket.
func BenchmarkSendInput(b *testing.B) {
	p := newPeer(PeerInfo{CleanID: "100000001"}, "", false, nopConn{}, metrics.New())
	defer p.close()
	msg := map[string]any{"type": "input-command", "event": map[string]any{"action": "mousemove", "normX": 0.5, "normY": 0.5}}
	b.ReportAllocs()
	for b.Loop() {
		if err := p.Send(msg); err != nil {
			b.Fatal(err)
		}
	}
}
