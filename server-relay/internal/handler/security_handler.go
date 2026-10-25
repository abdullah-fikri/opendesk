package handler

import (
	"context"
	"encoding/json"
	"errors"
	"strings"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/usecase"
)

// onSecurity handles every administrator request about the device allowlist. Each change is answered with the
// new state, sent to every connected administrator.
func (c *connection) onSecurity(kind string, msg []byte) {
	if !c.peer.IsAdmin {
		c.h.log.Warn("security request refused: not an administrator", "id", c.peer.DeskID, "type", kind)
		return
	}
	var change usecase.SecurityChange
	var err error
	switch kind {
	case dto.TypeSecurityGet:
		if state, ok := c.h.securityState(c.peer); ok {
			_ = c.peer.Send(state)
		}
		return
	case dto.TypeSecuritySet:
		var req dto.SecuritySet
		if json.Unmarshal(msg, &req) != nil {
			return
		}
		change, err = c.h.relay.SetEnforced(c.peer, req.Enabled)
	case dto.TypeSecurityAllow:
		var req dto.SecurityAllow
		if json.Unmarshal(msg, &req) != nil {
			return
		}
		change, err = c.h.relay.AllowDevice(c.peer, model.CleanDeskID(req.TargetID), req.Admin)
	case dto.TypeSecurityAllowOnline:
		change, err = c.h.relay.AllowConnected(c.peer)
	case dto.TypeSecurityRemove, dto.TypeSecurityReset:
		var req dto.SecurityTarget
		if json.Unmarshal(msg, &req) != nil {
			return
		}
		target := model.CleanDeskID(req.TargetClean)
		if kind == dto.TypeSecurityRemove {
			change, err = c.h.relay.RemoveDevice(c.peer, target)
		} else {
			change, err = c.h.relay.ResetDevice(c.peer, target)
		}
	}
	if errors.Is(err, usecase.ErrInvalidDeskID) {
		_ = c.peer.Send(dto.Error{Type: dto.TypeError, Message: msgInvalidDeskID})
		return
	}
	if err != nil {
		return
	}
	c.h.applySecurityChange(change)
	c.h.broadcastSecurityState()
}

// onHostRename names a device for every administrator.
func (c *connection) onHostRename(msg []byte) {
	var req dto.HostRename
	if json.Unmarshal(msg, &req) != nil {
		return
	}
	err := c.h.relay.SetAlias(c.peer, model.CleanDeskID(req.TargetClean), strings.TrimSpace(req.Alias))
	switch {
	case errors.Is(err, usecase.ErrNotAdmin):
		c.h.log.Warn("rename refused: not an administrator", "id", c.peer.DeskID)
		return
	case err != nil:
		c.h.log.Error("rename failed", "target", req.TargetClean, "err", err)
		return
	}
	c.h.broadcastHostList()
	c.h.broadcastSecurityState()
}

// applySecurityChange delivers fresh tokens and disconnects devices that are no longer admitted. An unlisted
// device sees the same message on every reconnect attempt.
func (h *Handler) applySecurityChange(change usecase.SecurityChange) {
	for _, it := range change.Issued {
		_ = it.Peer.Send(dto.DeviceCredential{Type: dto.TypeDeviceCredential, Token: it.Token})
	}
	for _, p := range change.Kicked {
		h.log.Warn("device disconnected: not on the device allowlist", "id", p.DeskID)
		_ = p.Send(dto.Error{Type: dto.TypeError, Message: msgInvalidCredentials})
		_ = p.Disconnect()
	}
}

// broadcastSecurityState pushes the allowlist to every administrator, off the caller's goroutine.
func (h *Handler) broadcastSecurityState() {
	go func() {
		for _, admin := range h.relay.Admins() {
			if state, ok := h.securityState(admin); ok {
				_ = admin.Send(state)
			}
		}
	}()
}

// securityState is the allowlist as admin sees it, with names from the registry when it is reachable.
func (h *Handler) securityState(admin *usecase.Peer) (dto.SecurityState, bool) {
	view, err := h.relay.Security(admin)
	if err != nil {
		return dto.SecurityState{}, false
	}
	hosts, err := h.relay.HostList(context.Background())
	if err != nil {
		h.log.Error("host list query failed", "err", err)
	}
	return securityStateDTO(view, hosts, func(id string) bool { return h.relay.Lookup(id) != nil }), true
}
