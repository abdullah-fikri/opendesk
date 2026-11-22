package usecase

import (
	"crypto/subtle"
	"encoding/json"
	"slices"
	"strings"
	"time"
)

// File lane (see docs/features/relay-file-lane.md at the repository root): a second WebSocket of a registered
// device that carries only file transfers. On the main connection files share one socket and one outbox with
// video, voice and input, and wait behind them (outbox.go); a lane has its own socket, its own writer and a large
// file queue with nothing ahead of it, so a transfer runs at the speed of the two TCP legs while the session's
// video keeps its latency.
//
// Security: a lane is not a registration. It can only attach to a device whose main connection is current (not
// replaced, suspended or held) and listed file-lane, with the one-time token the relay gave that main connection
// moments before (IssueLaneToken): a stranger without the token, or with an old one, cannot attach to anyone's
// lane. Everything on a lane is authorized by routeFile exactly like main-connection traffic (an active session
// to start, then the transfer's grant), as the lane's owner; only start, chunk and end are accepted. The lane dies
// with its main connection.
//
// Order: a transfer stays on the connection it started on, on both sides (fileGrant.via and fileGrant.out), and
// every transfer from one lane to one receiver uses the same receiver connection (laneOutLocked), so the
// receiver gets a lane's chunks in the order they were sent.
//
// A lane that drops cuts the transfers on it: their grants go, and the partner on the other end learns which
// ones on its main connection (file-lane-lost), so both sides abort them and the resume machinery continues them.
const (
	// laneTokenTTL: a lane token must be used this soon after it was issued, once.
	laneTokenTTL = 30 * time.Second
	// laneQueueLen and laneQueueBytes bound what waits for one receiving lane. Larger than the main file queue
	// (fileQueueBytes), since nothing else shares the socket: room for the bandwidth-delay product of a fast
	// far relay-to-receiver leg. A full queue holds the sending lane's read loop back (and with it, through TCP,
	// the sender), so one slow receiver costs the relay at most this much memory.
	laneQueueLen   = 256
	laneQueueBytes = 16 << 20
)

// Lane is the file-only second connection of a registered peer.
type Lane struct {
	// Owner is the registration the lane belongs to; it never changes.
	Owner *Peer
	// out is the lane's outbox: its own writer goroutine and a bounded file queue (see outbox.go); nothing but
	// file traffic and the attach answer is ever queued on it.
	out *Peer
}

// LaneLost names, for one partner, the transfers a dropped lane cut (see file-lane-lost).
type LaneLost struct {
	// To receives the notice; From is the clean ID of the device whose lane dropped.
	To   *Peer
	From string
	IDs  []string
}

// newLane starts the writer of a lane of owner on conn.
func newLane(owner *Peer, conn Conn, m FrameMetrics) *Lane {
	out := &Peer{
		CleanID:    owner.CleanID,
		DeskID:     owner.DeskID,
		FileBinary: true,
		conn:       conn,
		metrics:    m,
		control:    newMsgRing(controlQueueLen),
		file:       newMsgRing(laneQueueLen),
		room:       make(chan struct{}),
		wake:       make(chan struct{}, 1),
		closeCh:    make(chan struct{}),
		writerDone: make(chan struct{}),
	}
	go out.writeLoop()
	return &Lane{Owner: owner, out: out}
}

// Send queues a control message (the attach answer) on the lane.
func (l *Lane) Send(v any) error { return l.out.Send(v) }

// SendFile queues one file-transfer message from the given sender; it waits for room like Peer.SendFile.
func (l *Lane) SendFile(from string, v any) error {
	data, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return l.out.push(&l.out.file, laneQueueBytes, fileStallLimit, outMsg{text: data, from: from})
}

// SendFileChunk queues a binary file chunk from the given sender, tagged with its clean ID like on the main
// connection; it waits for room like SendFile.
func (l *Lane) SendFileChunk(from string, chunk []byte) error {
	return l.out.push(&l.out.file, laneQueueBytes, fileStallLimit, outMsg{bin: chunk, marker: sessionTag, tag: from, from: from})
}

// Ping sends a keep-alive; safe to call concurrently with the writer.
func (l *Lane) Ping() error { return l.out.Ping() }

// Close stops the lane's writer and closes its connection, which ends its read loop (and then DetachLane runs).
func (l *Lane) Close() { l.out.kill() }

// FileLaneEnabled reports whether the relay advertises file-lane.
func (r *Relay) FileLaneEnabled() bool { return r.fileLane }

// IssueLaneToken returns a new one-time lane token for p's main connection; it replaces any earlier one and is
// valid for laneTokenTTL. Only a current, active registration that listed file-lane gets one.
func (r *Relay) IssueLaneToken(p *Peer) (string, error) {
	if !r.fileLane || p == nil || !p.FileLane {
		return "", ErrUnauthorized
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.isCurrentLocked(p) || p.quietLocked() {
		return "", ErrUnauthorized
	}
	p.laneToken = newResumeToken()
	p.laneTokenExpires = time.Now().Add(laneTokenTTL)
	return p.laneToken, nil
}

// AttachLane makes conn the file lane of the device cleanID, which must be registered with an active main
// connection that holds the unexpired token. The token is spent. A lane the device had before is replaced; the
// returned notices name the transfers that ran on it.
func (r *Relay) AttachLane(conn Conn, cleanID, token string) (*Lane, []LaneLost, error) {
	now := time.Now()
	r.mu.Lock()
	defer r.mu.Unlock()
	owner := r.peers[cleanID]
	if !r.fileLane || owner == nil || !owner.FileLane || owner.quietLocked() || owner.laneToken == "" ||
		now.After(owner.laneTokenExpires) || subtle.ConstantTimeCompare([]byte(token), []byte(owner.laneToken)) != 1 {
		return nil, nil, ErrUnauthorized
	}
	owner.laneToken = ""
	lost := r.closeLaneLocked(owner)
	lane := newLane(owner, conn, r.metrics)
	owner.lane = lane
	return lane, lost, nil
}

// DetachLane runs when l's connection ended. If l was still its owner's lane, the transfers on it are cut; the
// returned notices tell their partners.
func (r *Relay) DetachLane(l *Lane) []LaneLost {
	l.Close()
	r.mu.Lock()
	defer r.mu.Unlock()
	if l.Owner.lane != l {
		return nil
	}
	return r.closeLaneLocked(l.Owner)
}

// closeLaneLocked closes p's lane, if any, and drops the grants of every transfer that ran on it (either way),
// returning the notices for the partners. Requires the write lock.
func (r *Relay) closeLaneLocked(p *Peer) []LaneLost {
	l := p.lane
	if l == nil {
		return nil
	}
	p.lane = nil
	l.Close()
	byPartner := make(map[*Peer][]string)
	for key, gr := range r.grants {
		var partner *Peer
		switch {
		case gr.via == l:
			partner = gr.to
		case gr.out == l:
			partner = gr.from
		default:
			continue
		}
		delete(r.grants, key)
		if r.isCurrentLocked(partner) {
			byPartner[partner] = append(byPartner[partner], gr.id)
		}
	}
	lost := make([]LaneLost, 0, len(byPartner))
	for partner, ids := range byPartner {
		slices.Sort(ids)
		lost = append(lost, LaneLost{To: partner, From: p.CleanID, IDs: ids})
	}
	slices.SortFunc(lost, func(a, b LaneLost) int { return strings.Compare(a.To.CleanID, b.To.CleanID) })
	return lost
}

// laneOutLocked picks the receiver connection of a new transfer from lane via to to: to's lane when it has one,
// except while other transfers from the same lane to to still run, which keep their connection. So the chunks of
// one lane reach a receiver over one connection, in the order they were sent (clients number them per lane when
// they seal them end to end, and reject one that arrives after a later one). Requires mu.
func (r *Relay) laneOutLocked(via *Lane, to *Peer) *Lane {
	for _, gr := range r.grants {
		if gr.via == via && gr.to == to {
			return gr.out
		}
	}
	return to.lane
}

// RouteLaneFile authorizes a JSON file message that came on lane l (see routeFile) and returns its receiver, or
// nil, and the receiver's lane to deliver it on (nil: the receiver's main connection). Only start, chunk and end
// travel on a lane; offers, pulls and acknowledgements stay on the main connection.
func (r *Relay) RouteLaneFile(l *Lane, targetClean, action, id string) (*Peer, *Lane) {
	switch action {
	case FileStart, FileChunk, FileEnd:
		return r.routeFile(l.Owner, l, targetClean, action, id)
	}
	r.metrics.Rejected()
	return nil, nil
}

// ForwardLaneBinary forwards a binary file chunk that came on lane l: sessionTag, the receiver's clean ID, then
// the chunk (a lane may serve several sessions, so the tag is required). It is authorized like a chunk on the
// main connection (forwardFileChunk) and goes out tagged with the owner's clean ID. It may wait for room in the
// receiver's queue, which holds the lane's read loop back.
func (r *Relay) ForwardLaneBinary(l *Lane, msg []byte) bool {
	if len(msg) <= sessionTagLen || msg[0] != sessionTag || msg[sessionTagLen] != fileChunkMarker {
		r.metrics.Rejected()
		return false
	}
	target, chunk := string(msg[1:sessionTagLen]), msg[sessionTagLen:]
	rcpt, out := r.routeFile(l.Owner, l, target, FileChunk, fileChunkID(chunk))
	if rcpt == nil || !rcpt.FileBinary {
		r.metrics.Rejected()
		return false
	}
	if out != nil {
		_ = out.SendFileChunk(l.Owner.CleanID, chunk)
	} else {
		_ = rcpt.SendFileChunk(l.Owner.CleanID, chunk)
	}
	return true
}

// Lanes returns a snapshot of the attached file lanes.
func (r *Relay) Lanes() []*Lane {
	r.mu.RLock()
	defer r.mu.RUnlock()
	var out []*Lane
	for _, p := range r.peers {
		if p.lane != nil {
			out = append(out, p.lane)
		}
	}
	return out
}
