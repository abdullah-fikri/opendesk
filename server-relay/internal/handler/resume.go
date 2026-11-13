package handler

import (
	"context"
	"time"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/usecase"
)

// resumeCheckInterval is how often suspended sessions are checked for an expired grace.
const resumeCheckInterval = time.Second

// RunResumeExpiry ends the suspended sessions whose grace is over (see docs/features/session-resume.md at the
// repository root) until ctx ends.
func (h *Handler) RunResumeExpiry(ctx context.Context) {
	ticker := time.NewTicker(resumeCheckInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case now := <-ticker.C:
			for _, e := range h.relay.ExpireSuspended(now) {
				h.endSuspension(e)
			}
		}
	}
}

// endSuspension tells the partners of a suspended connection that went offline that their session ended.
func (h *Handler) endSuspension(e usecase.Expired) {
	for _, partner := range e.Partners {
		h.releaseUDPRelay(partner.CleanID, e.Peer.CleanID)
		_ = partner.Send(sessionClosed(partner, e.Peer.CleanID, msgPeerOffline))
	}
	h.log.Info("suspended client went offline", "id", e.Peer.DeskID, "active_peers", h.relay.PeerCount())
	h.broadcastHostList()
}

// announceResume finishes a registration that took over sessions: the ack is already queued, so traffic may
// flow again; the new connection learns which partners are themselves still away, the others that it is back.
func (c *connection) announceResume(res usecase.RegisterResult) {
	peer := res.Peer
	for _, id := range res.Waiting {
		_ = peer.Send(dto.SessionSuspended{Type: dto.TypeSessionSuspended, TargetClean: id})
	}
	c.h.relay.Release(peer)
	for _, partner := range res.Back {
		_ = partner.Send(dto.SessionResumed{Type: dto.TypeSessionResumed, TargetClean: peer.CleanID})
	}
	if len(res.Resumed) > 0 {
		c.h.log.Info("client resumed sessions", "id", peer.DeskID, "sessions", len(res.Resumed))
	}
}
