package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/usecase"
)

// Administrator registry edits: deleting offline devices and managing communities (see
// docs/features/communities.md at the repository root). The usecase checks the administrator flag; the handler
// only rate-limits, decodes and broadcasts.

// User-facing messages for refused registry edits.
const (
	msgDeleteOnline   = "%d device(s) are online and were not deleted. Only offline devices can be deleted."
	msgInvalidName    = "Enter a name of 1 to 48 characters."
	msgNameTaken      = "That name is already used."
	msgNotFound       = "That community or group no longer exists."
	msgLimitReached   = "Limit reached: at most 64 communities, 64 groups per community and 500 devices per change."
	msgEditsTooFast   = "Too many changes at once. Wait a moment and try again."
	msgStorageFailure = "The relay could not save the change. Try again."
)

// Edits are cheap but each one broadcasts to every administrator: a burst covers a quick series of checkbox
// toggles, then one per editInterval.
const (
	editBurst    = 20
	editInterval = 100 * time.Millisecond
)

// editBucket is a token bucket like signalBucket, with the edit limits.
type editBucket struct {
	tokens float64
	at     time.Time
}

// take spends one token at now; false if none is left.
func (b *editBucket) take(now time.Time) bool {
	if b.at.IsZero() {
		b.tokens = editBurst
	} else {
		b.tokens = min(editBurst, b.tokens+float64(now.Sub(b.at))/float64(editInterval))
	}
	b.at = now
	if b.tokens < 1 {
		return false
	}
	b.tokens--
	return true
}

// onHostDelete removes offline devices from the registry for every administrator.
func (c *connection) onHostDelete(msg []byte) {
	if !c.peer.IsAdmin {
		c.h.log.Warn("host delete refused: not an administrator", "id", c.peer.DeskID)
		return
	}
	if !c.edits.take(time.Now()) {
		_ = c.peer.Send(dto.Error{Type: dto.TypeError, Message: msgEditsTooFast})
		return
	}
	var req dto.HostDelete
	if json.Unmarshal(msg, &req) != nil {
		return
	}
	res, err := c.h.relay.DeleteHosts(c.peer, req.TargetCleans)
	if res.Deleted > 0 {
		c.h.broadcastHostList()
		c.h.broadcastCommunityList()
	}
	switch {
	case len(res.Online) > 0:
		_ = c.peer.Send(dto.Error{Type: dto.TypeError, Message: fmt.Sprintf(msgDeleteOnline, len(res.Online))})
	case err != nil:
		c.replyEditError("host delete", err)
	}
}

// onCommunityEdit applies one community or group change and sends every administrator the new list.
func (c *connection) onCommunityEdit(kind string, msg []byte) {
	if !c.peer.IsAdmin {
		c.h.log.Warn("community request refused: not an administrator", "id", c.peer.DeskID, "type", kind)
		return
	}
	if kind == dto.TypeGetCommunities {
		if list, ok := c.h.communityList(c.peer); ok {
			_ = c.peer.Send(list)
		}
		return
	}
	if !c.edits.take(time.Now()) {
		_ = c.peer.Send(dto.Error{Type: dto.TypeError, Message: msgEditsTooFast})
		return
	}
	var req dto.CommunityEdit
	if json.Unmarshal(msg, &req) != nil {
		return
	}
	r, admin := c.h.relay, c.peer
	var err error
	switch kind {
	case dto.TypeCommunityCreate:
		_, err = r.CreateCommunity(admin, usecase.CommunityInput{Name: req.Name, Icon: req.Icon, Color: req.Color})
	case dto.TypeCommunityUpdate:
		err = r.UpdateCommunity(admin, req.CommunityID, usecase.CommunityInput{Name: req.Name, Icon: req.Icon, Color: req.Color})
	case dto.TypeCommunityDelete:
		err = r.DeleteCommunity(admin, req.CommunityID)
	case dto.TypeGroupCreate:
		_, err = r.CreateGroup(admin, req.CommunityID, req.Name)
	case dto.TypeGroupRename:
		err = r.RenameGroup(admin, req.GroupID, req.Name)
	case dto.TypeGroupDelete:
		err = r.DeleteGroup(admin, req.GroupID)
	case dto.TypeGroupMembers:
		err = r.SetGroupMembers(admin, req.GroupID, req.Add, req.Remove)
	}
	if err != nil {
		c.replyEditError(kind, err)
		return
	}
	c.h.log.Info("community changed", "type", kind, "by", admin.DeskID)
	c.h.broadcastCommunityList()
}

// replyEditError tells the administrator why its change was refused.
func (c *connection) replyEditError(kind string, err error) {
	message := msgStorageFailure
	switch {
	case errors.Is(err, usecase.ErrNotAdmin), errors.Is(err, usecase.ErrUnsupported):
		return
	case errors.Is(err, usecase.ErrInvalidDeskID):
		message = msgInvalidDeskID
	case errors.Is(err, usecase.ErrInvalidName):
		message = msgInvalidName
	case errors.Is(err, usecase.ErrNameTaken):
		message = msgNameTaken
	case errors.Is(err, usecase.ErrNotFound):
		message = msgNotFound
	case errors.Is(err, usecase.ErrLimit):
		message = msgLimitReached
	default:
		c.h.log.Error("registry edit failed", "type", kind, "err", err)
	}
	_ = c.peer.Send(dto.Error{Type: dto.TypeError, Message: message})
}

// broadcastCommunityList pushes the communities to every administrator, off the caller's goroutine.
func (h *Handler) broadcastCommunityList() {
	if !h.relay.CommunitiesEnabled() {
		return
	}
	go func() {
		admins := h.relay.Admins()
		if len(admins) == 0 {
			return
		}
		list, ok := h.communityList(admins[0])
		if !ok {
			return
		}
		for _, admin := range admins {
			_ = admin.Send(list)
		}
	}()
}

func (h *Handler) communityList(admin *usecase.Peer) (dto.CommunityList, bool) {
	all, err := h.relay.Communities(admin)
	if err != nil {
		if !errors.Is(err, usecase.ErrUnsupported) {
			h.log.Error("community list query failed", "err", err)
		}
		return dto.CommunityList{}, false
	}
	return communityListDTO(all), true
}

// communityListDTO maps communities for the wire; empty lists are sent as [] rather than null.
func communityListDTO(all []model.Community) dto.CommunityList {
	out := dto.CommunityList{Type: dto.TypeCommunityList, Communities: make([]dto.Community, len(all))}
	for i, c := range all {
		groups := make([]dto.Group, len(c.Groups))
		for j, g := range c.Groups {
			members := g.Members
			if members == nil {
				members = []string{}
			}
			groups[j] = dto.Group{ID: g.ID, Name: g.Name, Members: members}
		}
		out.Communities[i] = dto.Community{ID: c.ID, Name: c.Name, Icon: c.Icon, Color: c.Color, Groups: groups}
	}
	return out
}
