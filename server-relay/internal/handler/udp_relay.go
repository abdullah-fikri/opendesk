package handler

import (
	"slices"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/udprelay"
	"opendesk/server-relay/internal/usecase"
)

// offerUDPRelay allocates a session's UDP relay pair and sends each side its bind token, when this relay runs
// the forwarder and both clients can use it. Call it after both session-connected messages, so a client already
// has the session the offer names.
func (h *Handler) offerUDPRelay(viewer, host *usecase.Peer) {
	fwd := h.opts.UDPRelay
	if fwd == nil || !h.relay.P2PEnabled() || !canUseUDPRelay(viewer) || !canUseUDPRelay(host) {
		return
	}
	tv, th, err := fwd.Allocate(udprelay.PairKey(viewer.CleanID, host.CleanID))
	if err != nil {
		h.log.Warn("UDP relay allocation failed", "err", err)
		return
	}
	addr := h.opts.UDPRelayPublic
	_ = viewer.Send(dto.UDPRelayOffer{Type: dto.TypeUDPRelayOffer, TargetClean: host.CleanID, Addr: addr, Token: tv.String()})
	_ = host.Send(dto.UDPRelayOffer{Type: dto.TypeUDPRelayOffer, TargetClean: viewer.CleanID, Addr: addr, Token: th.String()})
}

// releaseUDPRelay drops the UDP relay pair of the session between a and b, if any.
func (h *Handler) releaseUDPRelay(a, b string) {
	if h.opts.UDPRelay != nil {
		h.opts.UDPRelay.Release(udprelay.PairKey(a, b))
	}
}

func canUseUDPRelay(p *usecase.Peer) bool {
	return slices.Contains(p.Caps, model.CapUDPRelay) && slices.Contains(p.Caps, model.CapP2P)
}
