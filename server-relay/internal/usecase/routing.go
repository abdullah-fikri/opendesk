package usecase

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
