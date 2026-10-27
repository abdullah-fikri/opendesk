package usecase

import (
	"encoding/json"
	"slices"
	"time"
)

// Outbound side of a peer. One writer goroutine (writeLoop) owns the socket and serves the queues below in
// priority order: control and input text, then voice, then video (the frame queues), then file transfers.
// Senders only enqueue, so a receiver whose TCP window is full never stalls the read loop of whoever sends to
// it for long. Order is kept within each class. Text overtaking queued video or voice is safe: nothing a client
// does depends on it, and a session starts (session-connected) before its host can produce a frame.
//
// A full control or file queue makes the sender wait for room while the writer is making progress (a burst to
// a healthy receiver, or a file sent faster than the receiver's link takes it). A receiver whose current write
// has been stuck for the class's stall limit is dead and is closed.
const (
	// controlQueueLen and controlQueueBytes bound queued text. One message larger than the byte bound (a large
	// clipboard text) is still taken when nothing else is queued.
	controlQueueLen   = 256
	controlQueueBytes = 16 << 20
	// controlStallLimit: input must never be dropped silently, so a receiver with a full control queue whose
	// socket has taken nothing for this long is closed rather than holding its sender back.
	controlStallLimit = 500 * time.Millisecond
	// audioQueueLen bounds queued voice; the oldest packet goes first, since late voice is worse than a gap.
	audioQueueLen = 16
	// fileQueueLen and fileQueueBytes bound queued file-transfer messages (chunks are at most 32 KiB decoded).
	fileQueueLen   = 16
	fileQueueBytes = 4 << 20
	// fileStallLimit is the stall limit for file traffic, the same bound one socket write has
	// (handler.writeTimeout); it also caps any wait for room.
	fileStallLimit = 10 * time.Second
	// fileShare: a waiting file message is written at least once per this many other messages, so video that
	// fills the link cannot starve a transfer.
	fileShare = 8
)

// outMsg is one queued message: text, or binary that a non-empty tag prefixes with marker and a host's clean ID.
type outMsg struct {
	text   []byte
	bin    []byte
	marker byte
	tag    string
	// hangUp closes the connection once everything queued before it is written.
	hangUp bool
}

func (m outMsg) size() int {
	if m.tag != "" {
		return sessionTagLen + len(m.bin)
	}
	return len(m.text) + len(m.bin)
}

// msgRing is a fixed-capacity FIFO of queued messages.
type msgRing struct {
	buf     []outMsg
	head, n int
	bytes   int
}

func newMsgRing(capacity int) msgRing { return msgRing{buf: make([]outMsg, capacity)} }

func (r *msgRing) full() bool { return r.n == len(r.buf) }

func (r *msgRing) push(m outMsg) {
	r.buf[(r.head+r.n)%len(r.buf)] = m
	r.n++
	r.bytes += m.size()
}

func (r *msgRing) pop() (outMsg, bool) {
	if r.n == 0 {
		return outMsg{}, false
	}
	m := r.buf[r.head]
	r.buf[r.head] = outMsg{}
	r.head = (r.head + 1) % len(r.buf)
	r.n--
	r.bytes -= m.size()
	return m, true
}

func (r *msgRing) clear() {
	for r.n > 0 {
		r.pop()
	}
}

// frameStream is one video queue the writer serves; a non-empty tag prefixes every frame with marker and tag.
type frameStream struct {
	q      *frameQueue
	marker byte
	tag    string
}

// addStream registers a new video queue with the writer.
func (p *Peer) addStream(marker byte, tag string) *frameQueue {
	q := newFrameQueue(p.metrics)
	q.wake = p.wake
	p.outMu.Lock()
	p.streams = append(p.streams, &frameStream{q: q, marker: marker, tag: tag})
	p.outMu.Unlock()
	return q
}

// removeStream unregisters a video queue and discards what it still holds.
func (p *Peer) removeStream(q *frameQueue) {
	p.outMu.Lock()
	p.streams = slices.DeleteFunc(p.streams, func(s *frameStream) bool { return s.q == q })
	p.outMu.Unlock()
	q.drain()
}

// Send encodes v as JSON and queues it as one control text message.
func (p *Peer) Send(v any) error {
	data, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return p.enqueue(outMsg{text: data})
}

// SendFile queues one file-transfer message with the given payload action. Offers and pulls are control
// traffic: a forwarded Ctrl+V relies on the offer having arrived before it. Everything else (start, chunks,
// end, ack) goes to the file queue in order. When that queue is full the caller (the sender's read loop) waits
// for room, as it waited on the socket before: senders have no end-to-end flow control besides TCP, so dropping
// or failing early would break transfers to a slower receiver.
func (p *Peer) SendFile(action string, v any) error {
	data, err := json.Marshal(v)
	if err != nil {
		return err
	}
	m := outMsg{text: data}
	if action == FileOffer || action == FilePull {
		return p.enqueue(m)
	}
	return p.push(&p.file, fileQueueBytes, fileStallLimit, m)
}

// Disconnect closes the connection after everything already queued for it is written (for example the error
// that explains why); its read loop then ends and the normal disconnect cleanup runs.
func (p *Peer) Disconnect() error {
	if p.enqueue(outMsg{hangUp: true}) != nil {
		return p.conn.Close()
	}
	return nil
}

// Ping sends a keep-alive; safe to call concurrently with the writer.
func (p *Peer) Ping() error { return p.conn.Ping() }

// enqueue adds one control message.
func (p *Peer) enqueue(m outMsg) error {
	return p.push(&p.control, controlQueueBytes, controlStallLimit, m)
}

// push adds m to q. While q is full it waits for the writer to take a message; when the writer's current write
// has been stuck for stall (or the wait exceeds fileStallLimit) the receiver is closed instead.
func (p *Peer) push(q *msgRing, maxBytes int, stall time.Duration, m outMsg) error {
	started := time.Now()
	p.outMu.Lock()
	for {
		if p.closed {
			p.outMu.Unlock()
			return ErrPeerClosed
		}
		if q.n == 0 || (!q.full() && q.bytes+m.size() <= maxBytes) {
			q.push(m)
			p.outMu.Unlock()
			p.signal()
			return nil
		}
		now := time.Now()
		wait := fileStallLimit - now.Sub(started)
		if !p.busySince.IsZero() {
			wait = min(wait, stall-now.Sub(p.busySince))
		}
		if wait <= 0 {
			p.outMu.Unlock()
			p.metrics.SlowReceiver()
			p.kill()
			return ErrSlowReceiver
		}
		room := p.room
		p.roomWaiters++
		p.outMu.Unlock()
		timer := time.NewTimer(wait)
		select {
		case <-room:
		case <-timer.C:
		case <-p.closeCh:
		}
		timer.Stop()
		p.outMu.Lock()
		p.roomWaiters--
		select {
		case <-p.closeCh:
			p.outMu.Unlock()
			return ErrPeerClosed
		default:
		}
	}
}

// deliverAudio queues a voice packet, tagged with its host when it goes to a multi-session viewer.
func (p *Peer) deliverAudio(view *viewSession, from string, data []byte) {
	m := outMsg{bin: data}
	if view != nil {
		m.marker, m.tag = sessionTag, from
	}
	p.outMu.Lock()
	if p.closed {
		p.outMu.Unlock()
		return
	}
	if p.audio.full() {
		p.audio.pop()
		p.metrics.DroppedAudio()
	}
	p.audio.push(m)
	p.outMu.Unlock()
	p.signal()
}

// deliverVideo queues a video frame on the session's own queue (multi-session) or the peer's queue.
func (p *Peer) deliverVideo(view *viewSession, data []byte) {
	if view != nil {
		view.frames.push(data)
		return
	}
	p.frames.push(data)
}

// signal wakes the writer; a wake-up already pending covers this one too.
func (p *Peer) signal() {
	select {
	case p.wake <- struct{}{}:
	default:
	}
}

// kill closes a receiver that cannot keep up or whose socket failed: the writer stops at once and the closed
// connection ends its read loop, which runs the normal disconnect cleanup.
func (p *Peer) kill() {
	p.close()
	_ = p.conn.Close()
}

// writeLoop is the peer's only socket writer. It ends when the peer closes or a write fails.
func (p *Peer) writeLoop() {
	defer p.release()
	for {
		p.outMu.Lock()
		m, ok := p.nextLocked()
		p.busySince = time.Time{}
		if ok {
			p.busySince = time.Now()
			if p.roomWaiters > 0 {
				close(p.room)
				p.room = make(chan struct{})
			}
		}
		p.outMu.Unlock()
		if !ok {
			select {
			case <-p.closeCh:
				return
			case <-p.wake:
				continue
			}
		}
		select {
		case <-p.closeCh:
			return
		default:
		}
		if m.hangUp || p.writeMsg(m) != nil {
			p.kill()
			return
		}
	}
}

// nextLocked picks the next message by class priority. Requires outMu.
func (p *Peer) nextLocked() (outMsg, bool) {
	if m, ok := p.control.pop(); ok {
		return p.notFileLocked(m), true
	}
	if p.file.n > 0 && p.sinceFile >= fileShare {
		return p.popFileLocked(), true
	}
	if m, ok := p.audio.pop(); ok {
		return p.notFileLocked(m), true
	}
	// Round robin over the video queues, so one session's frames never starve another's.
	for i := range p.streams {
		s := p.streams[(p.nextStream+i)%len(p.streams)]
		select {
		case frame := <-s.q.ch:
			p.nextStream = (p.nextStream + i + 1) % len(p.streams)
			return p.notFileLocked(outMsg{bin: frame, marker: s.marker, tag: s.tag}), true
		default:
		}
	}
	if p.file.n > 0 {
		return p.popFileLocked(), true
	}
	return outMsg{}, false
}

func (p *Peer) notFileLocked(m outMsg) outMsg {
	if p.file.n > 0 {
		p.sinceFile++
	}
	return m
}

// popFileLocked takes the oldest file message. Requires outMu.
func (p *Peer) popFileLocked() outMsg {
	m, _ := p.file.pop()
	p.sinceFile = 0
	return m
}

// writeMsg performs one socket write; the Conn bounds how long it may take.
func (p *Peer) writeMsg(m outMsg) error {
	var err error
	switch {
	case m.text != nil:
		err = p.conn.WriteText(m.text)
	case m.tag != "":
		var hdr [sessionTagLen]byte
		hdr[0] = m.marker
		copy(hdr[1:], m.tag)
		err = p.conn.WriteBinary(hdr[:], m.bin)
	default:
		err = p.conn.WriteBinary(m.bin)
	}
	if err != nil {
		return err
	}
	p.metrics.Egress(m.size())
	return nil
}

// release runs when the writer ends: later messages are refused and everything queued is discarded.
func (p *Peer) release() {
	p.outMu.Lock()
	p.closed = true
	p.control.clear()
	p.audio.clear()
	p.file.clear()
	streams := p.streams
	p.streams = nil
	p.outMu.Unlock()
	for _, s := range streams {
		s.q.drain()
	}
	close(p.writerDone)
}
