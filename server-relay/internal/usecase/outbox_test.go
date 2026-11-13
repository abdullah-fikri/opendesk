package usecase

import (
	"encoding/json"
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
	_ = p.SendFile("200000001", FileChunk, "chunk1")
	_ = p.SendFile("200000001", FileChunk, "chunk2")
	p.deliverVideo(view, key)
	p.deliverAudio(view, "200000001", []byte{audioMarker, 48, 1})
	p.deliverAudio(nil, "", []byte{audioMarker, 48, 2})
	_ = p.Send("c1")
	_ = p.SendFile("200000001", FileOffer, "offer") // control class
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
		if err := p.SendFile("200000001", FileChunk, i); err != nil {
			t.Fatal(err)
		}
	}
	sent := make(chan error, 1)
	go func() { sent <- p.SendFile("200000001", FileEnd, "end") }()
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

// Clipboard text never overtakes file messages its sender queued before it (a late end would replace the newer
// text on the receiver's clipboard); without such messages, or from another sender, it stays control traffic.
func TestClipboardWaitsBehindSendersFiles(t *testing.T) {
	conn := newGateConn(true)
	p := newPeer(PeerInfo{CleanID: "100000001"}, "", false, conn, metrics.New())
	defer p.close()
	_ = p.Send("hold") // the writer waits on the gate with this one
	time.Sleep(20 * time.Millisecond)
	_ = p.SendFile("200000001", FileChunk, "chunk")
	_ = p.SendFileChunk("200000001", []byte{0x12, 1, 'x', 0, 0, 0, 0, 0, 0, 0, 0})
	_ = p.SendFile("200000001", FileEnd, "end")
	_ = p.SendClipboard("200000001", "text-a")
	_ = p.SendClipboard("200000002", "text-b")
	_ = p.Send("input")
	close(conn.gate)

	got := conn.waitEntries(t, 7)
	want := []string{
		`t:"hold"`, `t:"text-b"`, `t:"input"`, `t:"chunk"`,
		fmt.Sprintf("b:%q", "\x10200000001\x12\x01x\x00\x00\x00\x00\x00\x00\x00\x00"), `t:"end"`, `t:"text-a"`,
	}
	if fmt.Sprint(got) != fmt.Sprint(want) {
		t.Fatalf("write order\n got %v\nwant %v", got, want)
	}
	// Nothing queued from the sender any more: text is control traffic again.
	_ = p.SendClipboard("200000001", "text-c")
	if e := conn.waitEntries(t, 8); e[7] != `t:"text-c"` {
		t.Fatalf("late text: %v", e)
	}
}

// The hot text path to a receiver: one relayed mouse move, recognized, encoded and handed to the receiver's
// socket.
func BenchmarkSendInput(b *testing.B) {
	r := newTestRelay(Options{})
	sender := buildPeer(PeerInfo{CleanID: "100000002"}, "", false, nopConn{}, r.metrics)
	p := newPeer(PeerInfo{CleanID: "100000001"}, "", false, nopConn{}, r.metrics)
	defer p.close()
	event, msg := inputCommand(map[string]any{"action": "mousemove", "normX": 0.5, "normY": 0.5})
	b.ReportAllocs()
	for b.Loop() {
		if err := r.SendInput(sender, p, event, msg); err != nil {
			b.Fatal(err)
		}
	}
}

// inputCommand returns a relayed input event and the input-command the handler queues for it.
func inputCommand(event map[string]any) (json.RawMessage, map[string]any) {
	raw, _ := json.Marshal(event)
	return raw, map[string]any{"type": "input-command", "event": event}
}

func sendInput(t *testing.T, r *Relay, sender, rcpt *Peer, event map[string]any) {
	t.Helper()
	raw, v := inputCommand(event)
	if err := r.SendInput(sender, rcpt, raw, v); err != nil {
		t.Fatalf("input %v refused: %v", event, err)
	}
}

func move(seq int) map[string]any {
	return map[string]any{"action": "mousemove", "normX": 0.5, "normY": 0.5, "seq": seq}
}

func button(action string) map[string]any {
	return map[string]any{"action": action, "normX": 0.5, "normY": 0.5, "button": 0}
}

// written is how gateConn records v written as text.
func written(v any) string {
	data, _ := json.Marshal(v)
	return "t:" + string(data)
}

func inputEntry(event map[string]any) string {
	_, v := inputCommand(event)
	return written(v)
}

// A viewer keeps moving the pointer while the host's link takes nothing for 2 s: the moves share one queue
// slot, the sender never waits, the connection stays up, and the host gets the newest position.
func TestMouseMovesToStalledReceiverCoalesce(t *testing.T) {
	t.Parallel()
	r := newTestRelay(Options{})
	viewer := buildPeer(PeerInfo{CleanID: "100000001"}, "", false, nopConn{}, r.metrics)
	conn := newGateConn(true)
	host := newPeer(PeerInfo{CleanID: "200000001"}, "", false, conn, r.metrics)
	defer host.close()
	_ = host.Send("hold") // the writer waits on the gate with this one
	time.Sleep(20 * time.Millisecond)

	const moves = 2000
	started := time.Now()
	for i := range moves {
		sendInput(t, r, viewer, host, move(i))
		if i%100 == 99 {
			time.Sleep(100 * time.Millisecond)
		}
	}
	if stalled := time.Since(started); stalled < 1800*time.Millisecond {
		t.Fatalf("receiver stalled only %v", stalled)
	}
	host.outMu.Lock()
	slots := host.control.n
	host.outMu.Unlock()
	if slots != 1 {
		t.Fatalf("moves took %d queue slots, want 1", slots)
	}
	close(conn.gate)
	conn.waitEntries(t, 2)
	time.Sleep(20 * time.Millisecond)
	want := []string{`t:"hold"`, inputEntry(move(moves - 1))}
	if got := conn.entries(); fmt.Sprint(got) != fmt.Sprint(want) || conn.isClosed() {
		t.Fatalf("writes %v closed=%v, want %v", got, conn.isClosed(), want)
	}
}

// Moves merge only with a move queued after everything else: clicks, keys and other senders' messages keep
// their place, so move, down, move, up reaches the host in that order. Sealed (e2e) events are never merged,
// and the emergency switch keeps every move.
func TestMouseMoveCoalescingKeepsOrder(t *testing.T) {
	run := func(t *testing.T, opts Options, events func(r *Relay, a, b, host *Peer)) []string {
		t.Helper()
		r := newTestRelay(opts)
		a := buildPeer(PeerInfo{CleanID: "100000001"}, "", false, nopConn{}, r.metrics)
		b := buildPeer(PeerInfo{CleanID: "100000002"}, "", false, nopConn{}, r.metrics)
		conn := newGateConn(true)
		host := newPeer(PeerInfo{CleanID: "200000001"}, "", false, conn, r.metrics)
		defer host.close()
		_ = host.Send("hold") // the writer waits on the gate with this one
		time.Sleep(20 * time.Millisecond)
		events(r, a, b, host)
		host.outMu.Lock()
		n := host.control.n + 1
		host.outMu.Unlock()
		close(conn.gate)
		got := conn.waitEntries(t, n)
		time.Sleep(20 * time.Millisecond)
		if e := conn.entries(); len(e) != len(got) {
			t.Fatalf("more writes than queued messages: %v", e)
		}
		return got
	}
	check := func(t *testing.T, got []string, want ...string) {
		t.Helper()
		want = append([]string{`t:"hold"`}, want...)
		if fmt.Sprint(got) != fmt.Sprint(want) {
			t.Fatalf("writes\n got %v\nwant %v", got, want)
		}
	}

	t.Run("clicks split moves", func(t *testing.T) {
		got := run(t, Options{}, func(r *Relay, a, _, host *Peer) {
			for _, e := range []map[string]any{move(1), move(2), button("mousedown"), move(3), move(4), button("mouseup"), move(5), move(6)} {
				sendInput(t, r, a, host, e)
			}
		})
		check(t, got, inputEntry(move(2)), inputEntry(button("mousedown")), inputEntry(move(4)),
			inputEntry(button("mouseup")), inputEntry(move(6)))
	})
	t.Run("other traffic splits moves", func(t *testing.T) {
		got := run(t, Options{}, func(r *Relay, a, b, host *Peer) {
			sendInput(t, r, a, host, move(1))
			sendInput(t, r, b, host, move(2)) // another sender's move
			sendInput(t, r, a, host, move(3))
			_ = host.Send(move(4)) // not relayed input: never merged
			sendInput(t, r, a, host, move(5))
			sendInput(t, r, a, host, move(6))
		})
		check(t, got, inputEntry(move(1)), inputEntry(move(2)), inputEntry(move(3)), written(move(4)),
			inputEntry(move(6)))
	})
	t.Run("sealed events keep their slot", func(t *testing.T) {
		sealed := func(d string) map[string]any { return map[string]any{"action": "e2e", "d": d} }
		got := run(t, Options{}, func(r *Relay, a, _, host *Peer) {
			sendInput(t, r, a, host, sealed("x"))
			sendInput(t, r, a, host, sealed("y"))
		})
		check(t, got, inputEntry(sealed("x")), inputEntry(sealed("y")))
	})
	t.Run("switch off", func(t *testing.T) {
		got := run(t, Options{MoveCoalescingDisabled: true}, func(r *Relay, a, _, host *Peer) {
			sendInput(t, r, a, host, move(1))
			sendInput(t, r, a, host, move(2))
		})
		check(t, got, inputEntry(move(1)), inputEntry(move(2)))
	})
}

// Clicks are never merged or dropped: a receiver whose link stays stalled with a control queue full of them is
// still open within the stall limit and closed once it is exceeded, before 6 s.
func TestStalledReceiverWithQueuedClicksIsClosed(t *testing.T) {
	t.Parallel()
	r := newTestRelay(Options{})
	viewer := buildPeer(PeerInfo{CleanID: "100000001"}, "", false, nopConn{}, r.metrics)
	conn := newGateConn(true)
	host := newPeer(PeerInfo{CleanID: "200000001"}, "", false, conn, r.metrics)
	started := time.Now()
	// One click is with the writer, the rest fill the queue.
	for i := 0; i <= controlQueueLen; i++ {
		sendInput(t, r, viewer, host, button([]string{"mousedown", "mouseup"}[i%2]))
	}
	overflow := make(chan error, 1)
	go func() {
		raw, v := inputCommand(button("mousedown"))
		overflow <- r.SendInput(viewer, host, raw, v)
	}()
	select {
	case err := <-overflow:
		t.Fatalf("receiver given up after %v, within the stall limit: %v", time.Since(started), err)
	case <-time.After(2 * time.Second):
	}
	if conn.isClosed() {
		t.Fatal("receiver closed within the stall limit")
	}
	select {
	case err := <-overflow:
		if !errors.Is(err, ErrSlowReceiver) || !conn.isClosed() {
			t.Fatalf("err %v closed=%v", err, conn.isClosed())
		}
	case <-time.After(6*time.Second - time.Since(started)):
		t.Fatal("receiver stalled for 6 s was not closed")
	}
	waitWriterDone(t, host)
}
