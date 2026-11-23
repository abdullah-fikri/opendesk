package usecase

import "time"

// File-transfer payload actions (see docs/protocol.md at the repository root).
const (
	FileStart = "start" // opens a transfer; needs an active session and creates a grant
	FileChunk = "chunk" // needs the grant
	FileEnd   = "end"   // needs the grant
	FileAck   = "ack"   // receiver -> sender; consumes the grant
	FileOffer = "offer" // clipboard file offer (names and sizes only); needs an active session
	FilePull  = "pull"  // asks the offerer to start a granted transfer; needs an active session
)

// Binary file chunk, used only between peers that both advertise file-binary (see docs/protocol.md):
// fileChunkMarker, id length (1 byte), id, offset (8 bytes, big endian), data. The transfer itself still starts
// and ends with JSON; a binary chunk needs the same grant as a JSON one.
const fileChunkMarker = 0x12

const (
	fileGrantIdle    = 60 * time.Second
	maxGrantsPerPeer = 8
	maxFileIDLen     = 80
)

// fileGrant lets one transfer finish after its screen session ends, but only between the same two connections.
type fileGrant struct {
	from, to *Peer
	id       string
	// via is the sender's file lane the transfer started on, nil for its main connection: every later message of
	// the transfer must come the same way (see lane.go).
	via *Lane
	// out is the receiver's file lane the transfer is delivered on, nil for its main connection; chosen at start.
	out     *Lane
	expires time.Time
}

func grantKey(fromClean, id string) string { return fromClean + ":" + id }

// RouteFile authorizes one file-transfer message from sender's main connection and returns its receiver, or nil.
func (r *Relay) RouteFile(sender *Peer, targetClean, action, id string) *Peer {
	rcpt, _ := r.routeFile(sender, nil, targetClean, action, id)
	return rcpt
}

// routeFile authorizes one file-transfer message from sender, which came on sender's file lane via (nil: on its
// main connection). It returns the receiver, or nil, and the receiver's lane the message goes to (nil: the
// receiver's main connection). Lane traffic passes exactly the checks main traffic does, plus: the lane must still
// be sender's, and a transfer never changes connection after its start.
func (r *Relay) routeFile(sender *Peer, via *Lane, targetClean, action, id string) (*Peer, *Lane) {
	if sender == nil || id == "" || len(id) > maxFileIDLen {
		return nil, nil
	}
	now := time.Now()
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.isCurrentLocked(sender) || (via != nil && sender.lane != via) {
		return nil, nil
	}
	switch action {
	case FileStart:
		target := r.activePartnerLocked(sender, targetClean)
		key := grantKey(sender.CleanID, id)
		if target == nil || r.grants[key] != nil || r.grantsOwnedLocked(sender) >= maxGrantsPerPeer {
			return nil, nil
		}
		gr := &fileGrant{from: sender, to: target, id: id, via: via, expires: now.Add(fileGrantIdle)}
		if via != nil {
			gr.out = r.laneOutLocked(via, target)
		}
		r.grants[key] = gr
		return target, gr.out
	case FileOffer, FilePull:
		return r.activePartnerLocked(sender, targetClean), nil
	case FileChunk, FileEnd:
		gr := r.grants[grantKey(sender.CleanID, id)]
		if gr == nil || gr.from != sender || r.peers[gr.to.CleanID] != gr.to || now.After(gr.expires) ||
			(targetClean != "" && targetClean != gr.to.CleanID) || gr.via != via || (gr.out != nil && gr.to.lane != gr.out) {
			return nil, nil
		}
		gr.expires = now.Add(fileGrantIdle)
		return gr.to, gr.out
	case FileAck:
		key := grantKey(targetClean, id)
		gr := r.grants[key]
		if gr == nil || gr.to != sender || r.peers[gr.from.CleanID] != gr.from || now.After(gr.expires) {
			return nil, nil
		}
		delete(r.grants, key)
		return gr.from, nil
	}
	return nil, nil
}

// fileChunkID returns the transfer ID of a binary file chunk, or "" when its header is malformed.
func fileChunkID(msg []byte) string {
	if len(msg) < 2 || msg[0] != fileChunkMarker {
		return ""
	}
	n := int(msg[1])
	if n == 0 || n > maxFileIDLen || len(msg) < 2+n+8 {
		return ""
	}
	return string(msg[2 : 2+n])
}

// forwardFileChunk authorizes a binary chunk exactly like a JSON one (the grant for this sender, transfer ID
// and receiver) and queues it as file traffic, tagged with the sender's clean ID: the binary counterpart of
// the fromClean the relay stamps on file-transfer. Receivers that did not advertise file-binary never get one.
func (r *Relay) forwardFileChunk(sender *Peer, targetClean string, msg []byte) bool {
	rcpt := r.RouteFile(sender, targetClean, FileChunk, fileChunkID(msg))
	if rcpt == nil || !rcpt.FileBinary {
		r.metrics.Rejected()
		return false
	}
	_ = rcpt.SendFileChunk(sender.CleanID, msg)
	return true
}

func (r *Relay) grantsOwnedLocked(p *Peer) int {
	n := 0
	for _, gr := range r.grants {
		if gr.from == p {
			n++
		}
	}
	return n
}

// dropGrantsLocked removes p's grants (p may be nil) and every expired grant. Requires the write lock.
func (r *Relay) dropGrantsLocked(p *Peer, now time.Time) {
	for key, gr := range r.grants {
		if (p != nil && (gr.from == p || gr.to == p)) || now.After(gr.expires) {
			delete(r.grants, key)
		}
	}
}
