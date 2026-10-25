package usecase

import (
	"encoding/json"
	"slices"
	"sync"

	"opendesk/server-relay/internal/model"
)

// PeerInfo is what a device declares about itself at registration.
type PeerInfo struct {
	CleanID    string
	Platform   string
	DeviceName string
	IsAdmin    bool
	Caps       []string
	RemoteAddr string
}

// Peer is one registered connection. Identity fields never change after registration; session fields are
// guarded by Relay.mu.
type Peer struct {
	CleanID    string
	DeskID     string
	Platform   string
	DeviceName string
	IsAdmin    bool
	Caps       []string
	// Multi is set when the client advertised the multi-session capability; it never changes afterwards.
	Multi bool
	// MonitorHost and MonitorView are set from the monitor-host and monitor-view capabilities.
	MonitorHost bool
	MonitorView bool
	// EnrollToken is the token issued at this registration (the device just enrolled); the caller sends it once.
	EnrollToken string

	// Single-session state: hosts, legacy viewers and Android peers (guarded by Relay.mu).
	sessionWith   string
	sessionActive bool
	// views holds a multi-session viewer's sessions keyed by host clean ID (guarded by Relay.mu).
	views map[string]*viewSession
	// watching holds an administrator's monitor-wall tiles keyed by host clean ID (guarded by Relay.mu).
	watching map[string]*monitorTile

	conn      Conn
	writeMu   sync.Mutex
	frames    *frameQueue
	closeCh   chan struct{}
	closeOnce sync.Once
	metrics   FrameMetrics
}

// viewSession is one session of a multi-session viewer. Each has its own frame queue so a key frame or
// congestion from one host never flushes another host's delta chain.
type viewSession struct {
	active bool // guarded by Relay.mu
	frames *frameQueue
	done   chan struct{}
}

func newPeer(info PeerInfo, deskID string, multi bool, conn Conn, m FrameMetrics) *Peer {
	p := &Peer{
		CleanID:     info.CleanID,
		DeskID:      deskID,
		Platform:    info.Platform,
		DeviceName:  info.DeviceName,
		IsAdmin:     info.IsAdmin,
		Caps:        info.Caps,
		Multi:       multi,
		MonitorHost: slices.Contains(info.Caps, model.CapMonitorHost),
		MonitorView: slices.Contains(info.Caps, model.CapMonitorView),
		conn:        conn,
		frames:      newFrameQueue(m),
		closeCh:     make(chan struct{}),
		metrics:     m,
	}
	go p.writeFrames(p.frames, nil, 0, "")
	return p
}

// close stops the peer's frame writers. It does not close the connection.
func (p *Peer) close() {
	p.closeOnce.Do(func() { close(p.closeCh) })
}

// writeFrames sends queued video until the peer (or the view, when done is non-nil) ends.
// A non-empty tag prefixes every frame with marker and that host's clean ID.
func (p *Peer) writeFrames(q *frameQueue, done <-chan struct{}, marker byte, tag string) {
	for {
		select {
		case <-p.closeCh:
			return
		case <-done:
			return
		case frame := <-q.ch:
			if tag == "" {
				_ = p.SendBinary(frame)
			} else {
				_ = p.sendTagged(marker, tag, frame)
			}
		}
	}
}

// startView creates a session queue whose writer tags every frame with the host's clean ID.
func (p *Peer) startView(hostClean string, active bool) *viewSession {
	v := &viewSession{active: active, frames: newFrameQueue(p.metrics), done: make(chan struct{})}
	go p.writeFrames(v.frames, v.done, sessionTag, hostClean)
	return v
}

func (v *viewSession) stop() {
	close(v.done)
	v.frames.drain()
}

// Send encodes v as JSON and writes it as one text message.
func (p *Peer) Send(v any) error {
	data, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return p.write(func() error { return p.conn.WriteText(data) }, len(data))
}

// SendBinary writes data as one binary message.
func (p *Peer) SendBinary(data []byte) error {
	return p.write(func() error { return p.conn.WriteBinary(data) }, len(data))
}

// sendTagged writes marker+hostClean+data as one binary message without copying data. The marker is
// sessionTag for multi-session traffic and monitorTag for monitor-wall frames.
func (p *Peer) sendTagged(marker byte, hostClean string, data []byte) error {
	var hdr [sessionTagLen]byte
	hdr[0] = marker
	copy(hdr[1:], hostClean)
	return p.write(func() error { return p.conn.WriteBinary(hdr[:], data) }, sessionTagLen+len(data))
}

// Disconnect closes the connection; its read loop ends and the normal disconnect cleanup runs.
func (p *Peer) Disconnect() error { return p.conn.Close() }

// Ping sends a keep-alive; safe to call concurrently with other writes.
func (p *Peer) Ping() error { return p.conn.Ping() }

// write serializes one message. A failed write leaves the connection unusable, so it is closed: that ends
// the peer's read loop and the normal disconnect cleanup runs.
func (p *Peer) write(send func() error, size int) error {
	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	if err := send(); err != nil {
		_ = p.conn.Close()
		return err
	}
	p.metrics.Egress(size)
	return nil
}

// deliverAudio forwards a voice packet, tagged with its host when it goes to a multi-session viewer.
func (p *Peer) deliverAudio(view *viewSession, from string, data []byte) {
	if view != nil {
		_ = p.sendTagged(sessionTag, from, data)
		return
	}
	_ = p.SendBinary(data)
}

// deliverVideo queues a video frame on the session's own queue (multi-session) or the peer's queue.
func (p *Peer) deliverVideo(view *viewSession, data []byte) {
	if view != nil {
		view.frames.push(data)
		return
	}
	p.frames.push(data)
}
