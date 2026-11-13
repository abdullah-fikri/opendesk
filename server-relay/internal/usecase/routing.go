package usecase

import (
	"bytes"
	"encoding/json"
)

// inputMouseMove is the input event action of a pointer move (docs/protocol.md, Input events).
const inputMouseMove = "mousemove"

// ActivePartner returns the receiver of a message from sender to targetClean, or nil unless both sides hold an
// active session with each other. An empty targetClean selects the single session of a non-multi peer.
func (r *Relay) ActivePartner(sender *Peer, targetClean string) *Peer {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return r.activePartnerLocked(sender, targetClean)
}

// ForwardBinary relays one binary message (voice, video or a file chunk) from sender to its session partner
// and reports whether it had an authorized receiver.
//
// A multi-session viewer addresses a host with the session-tag prefix, which is stripped (zero copy) so the
// host receives the plain payload. Messages to a multi-session viewer are tagged with the sending host's ID;
// its video goes through that session's own queue.
//
// A file chunk is routed by its transfer grant instead (see forwardFileChunk).
//
// A monitor-wall thumbnail (monitorTag + JPEG) is never session traffic: it goes only to the administrators
// watching the sender (see monitor.go).
func (r *Relay) ForwardBinary(sender *Peer, msg []byte) bool {
	if len(msg) > 1 && msg[0] == monitorTag {
		return r.fanOutMonitor(sender, msg[1:])
	}
	targetID := ""
	if sender.Multi && len(msg) > sessionTagLen && msg[0] == sessionTag {
		targetID, msg = string(msg[1:sessionTagLen]), msg[sessionTagLen:]
	}
	if len(msg) > 0 && msg[0] == fileChunkMarker {
		return r.forwardFileChunk(sender, targetID, msg)
	}
	r.mu.RLock()
	rcpt := r.activePartnerLocked(sender, targetID)
	var view *viewSession
	if rcpt != nil {
		view = rcpt.views[sender.CleanID]
	}
	r.mu.RUnlock()
	if rcpt == nil {
		r.metrics.Rejected()
		return false
	}
	switch {
	case isAudio(msg):
		rcpt.deliverAudio(view, sender.CleanID, msg)
	case isVideo(msg):
		rcpt.deliverVideo(view, msg)
	}
	return true
}

// SendInput queues v, the input-command that carries event from sender, for rcpt (the partner ActivePartner
// returned). A plain mouse move may be replaced by the sender's next one while both wait in rcpt's control
// queue, unless coalescing is disabled; every other event keeps its own slot and is never dropped. Events sealed
// end to end (action "e2e") are opaque to the relay and always keep their own slot.
func (r *Relay) SendInput(sender, rcpt *Peer, event json.RawMessage, v any) error {
	moveFrom := ""
	if r.mergeMoves && isMouseMove(event) {
		moveFrom = sender.CleanID
	}
	return rcpt.sendInput(moveFrom, v)
}

// isMouseMove reports whether a relayed input event is a plain mouse move.
//
// SECURITY: event comes from the sender and is not trusted. Only its action is read, and a move is keyed by the
// sender's own clean ID, so a forged "mousemove" can at most replace that same sender's queued move: it never
// touches another sender's messages or any non-move message.
func isMouseMove(event json.RawMessage) bool {
	// Most events are not moves (sealed e2e events, keys, clicks): skip the decode unless the word occurs.
	if !bytes.Contains(event, []byte(inputMouseMove)) {
		return false
	}
	var e struct {
		Action string `json:"action"`
	}
	return json.Unmarshal(event, &e) == nil && e.Action == inputMouseMove
}
