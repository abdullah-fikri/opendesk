package usecase

import (
	"slices"
	"sync"
	"time"

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
	// FileBinary is set from the file-binary capability: the client accepts binary file chunks.
	FileBinary bool
	// EnrollToken is the token issued at this registration (the device just enrolled); the caller sends it once.
	EnrollToken string

	// Single-session state: hosts, legacy viewers and Android peers (guarded by Relay.mu).
	sessionWith   string
	sessionActive bool
	// views holds a multi-session viewer's sessions keyed by host clean ID (guarded by Relay.mu).
	views map[string]*viewSession
	// watching holds an administrator's monitor-wall tiles keyed by host clean ID (guarded by Relay.mu).
	watching map[string]*monitorTile

	conn    Conn
	metrics FrameMetrics
	// frames is the single-session video queue (untagged).
	frames *frameQueue

	// Outbound queues served by writeLoop (see outbox.go), guarded by outMu.
	outMu                sync.Mutex
	control, audio, file msgRing
	streams              []*frameStream
	nextStream           int
	sinceFile            int
	// busySince is when the writer's current write started; zero while it is idle.
	busySince time.Time
	// room is closed and replaced when the writer takes a message while roomWaiters senders wait for room.
	room        chan struct{}
	roomWaiters int
	// closed is set once the writer has stopped; later messages are refused.
	closed bool

	wake       chan struct{}
	closeCh    chan struct{}
	closeOnce  sync.Once
	writerDone chan struct{}
}

// viewSession is one session of a multi-session viewer. Each has its own frame queue so a key frame or
// congestion from one host never flushes another host's delta chain.
type viewSession struct {
	active bool // guarded by Relay.mu
	frames *frameQueue
}

func newPeer(info PeerInfo, deskID string, multi bool, conn Conn, m FrameMetrics) *Peer {
	p := buildPeer(info, deskID, multi, conn, m)
	go p.writeLoop()
	return p
}

// buildPeer returns a peer whose writer is not running yet.
func buildPeer(info PeerInfo, deskID string, multi bool, conn Conn, m FrameMetrics) *Peer {
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
		FileBinary:  slices.Contains(info.Caps, model.CapFileBinary),
		conn:        conn,
		metrics:     m,
		control:     newMsgRing(controlQueueLen),
		audio:       newMsgRing(audioQueueLen),
		file:        newMsgRing(fileQueueLen),
		room:        make(chan struct{}),
		wake:        make(chan struct{}, 1),
		closeCh:     make(chan struct{}),
		writerDone:  make(chan struct{}),
	}
	p.frames = p.addStream(0, "")
	return p
}

// close stops the peer's writer, which releases its queues. It does not close the connection.
func (p *Peer) close() {
	p.closeOnce.Do(func() { close(p.closeCh) })
}

// startView creates a session queue whose frames the writer tags with the host's clean ID.
func (p *Peer) startView(hostClean string, active bool) *viewSession {
	return &viewSession{active: active, frames: p.addStream(sessionTag, hostClean)}
}
