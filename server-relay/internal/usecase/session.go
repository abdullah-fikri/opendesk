package usecase

import (
	"context"
	"slices"

	"opendesk/server-relay/internal/model"
)

// ConnectResult describes a session created by Connect.
type ConnectResult struct {
	Target *Peer
	// NeedsApproval is true for Android hosts: the session stays pending until the host answers.
	NeedsApproval bool
}

// Connect opens a session from caller to targetClean. Windows hosts are unattended and become active at once;
// Android hosts must approve. A multi-session viewer may hold up to model.MaxViewSessions sessions; every
// other peer holds one, and a busy peer cannot be taken as host.
func (r *Relay) Connect(caller *Peer, targetClean string) (ConnectResult, error) {
	if targetClean == caller.CleanID {
		return ConnectResult{}, ErrSelfConnect
	}
	target := r.Lookup(targetClean)
	if target == nil {
		return ConnectResult{}, ErrTargetUnavailable
	}
	if slices.Contains(target.Caps, model.CapViewerOnly) {
		return ConnectResult{}, ErrTargetViewerOnly
	}
	needsApproval := target.Platform == model.PlatformAndroid
	if !r.openSession(caller, target, !needsApproval) {
		return ConnectResult{}, ErrTargetUnavailable
	}
	status := model.SessionActive
	if needsApproval {
		status = model.SessionPending
	}
	r.store("start session", func(ctx context.Context) error {
		return r.sessions.Start(ctx, model.SessionRecord{
			ViewerCleanID: caller.CleanID, ViewerDeskID: caller.DeskID,
			HostCleanID: target.CleanID, HostDeskID: target.DeskID, Status: status,
		}, caller.Multi)
	})
	return ConnectResult{Target: target, NeedsApproval: needsApproval}, nil
}

// Approve activates the pending session between viewer and host (the approving Android device).
func (r *Relay) Approve(viewer, host *Peer) error {
	r.mu.Lock()
	ok := r.isCurrentLocked(viewer) && r.isCurrentLocked(host)
	if ok {
		ve, va := viewer.sessionLocked(host.CleanID)
		he, ha := host.sessionLocked(viewer.CleanID)
		ok = ve && he && !va && !ha
	}
	if ok {
		viewer.activateLocked(host.CleanID)
		host.activateLocked(viewer.CleanID)
	}
	r.mu.Unlock()
	if !ok {
		return ErrNoPendingSession
	}
	r.store("activate session", func(ctx context.Context) error {
		return r.sessions.UpdateStatus(ctx, viewer.CleanID, host.CleanID, model.SessionActive)
	})
	return nil
}

// Reject ends the pending session between host (the declining Android device) and viewerClean and returns
// the viewer to notify.
func (r *Relay) Reject(host *Peer, viewerClean string) (*Peer, error) {
	r.mu.Lock()
	viewer := r.peers[viewerClean]
	ok := r.isCurrentLocked(host) && viewer != nil
	if ok {
		he, ha := host.sessionLocked(viewerClean)
		ve, va := viewer.sessionLocked(host.CleanID)
		ok = he && !ha && ve && !va
	}
	if ok {
		viewer.endLocked(host.CleanID)
		host.endLocked(viewerClean)
	}
	r.mu.Unlock()
	if !ok {
		return nil, ErrNoPendingSession
	}
	r.store("reject session", func(ctx context.Context) error {
		return r.sessions.UpdateStatus(ctx, viewerClean, host.CleanID, model.SessionRejected)
	})
	return viewer, nil
}

// Disconnect ends only sender's session with targetClean (a multi-session viewer keeps its other views) and
// returns the partner to notify, or nil when there was no such session.
func (r *Relay) Disconnect(sender *Peer, targetClean string) *Peer {
	r.mu.Lock()
	if !r.isCurrentLocked(sender) {
		r.mu.Unlock()
		return nil
	}
	if exists, _ := sender.sessionLocked(targetClean); !exists {
		r.mu.Unlock()
		return nil
	}
	target := r.peers[targetClean]
	if target != nil {
		if exists, _ := target.sessionLocked(sender.CleanID); exists {
			target.endLocked(sender.CleanID)
		}
	}
	sender.endLocked(targetClean)
	r.mu.Unlock()

	if target == nil {
		return nil
	}
	if sender.Multi || target.Multi {
		// Close only this pair's row; the viewer's other sessions stay open.
		r.store("close session", func(ctx context.Context) error {
			if err := r.sessions.UpdateStatus(ctx, sender.CleanID, targetClean, model.SessionClosed); err != nil {
				return err
			}
			return r.sessions.UpdateStatus(ctx, targetClean, sender.CleanID, model.SessionClosed)
		})
	} else {
		r.store("close peer sessions", func(ctx context.Context) error { return r.sessions.CloseForPeer(ctx, sender.CleanID) })
	}
	return target
}

// openSession records a session between caller and target. Requires both to be current and the target idle.
func (r *Relay) openSession(caller, target *Peer, active bool) bool {
	r.mu.Lock()
	defer r.mu.Unlock()
	if caller == nil || target == nil || caller == target || !r.isCurrentLocked(caller) || !r.isCurrentLocked(target) ||
		caller.sessionWith != "" || target.busyLocked() {
		return false
	}
	if caller.Multi {
		if caller.views[target.CleanID] != nil || len(caller.views) >= model.MaxViewSessions {
			return false
		}
		if caller.views == nil {
			caller.views = make(map[string]*viewSession, model.MaxViewSessions)
		}
		caller.views[target.CleanID] = caller.startView(target.CleanID, active)
	} else {
		caller.sessionWith, caller.sessionActive = target.CleanID, active
	}
	target.sessionWith, target.sessionActive = caller.CleanID, active
	return true
}

func (r *Relay) isCurrentLocked(p *Peer) bool { return p != nil && r.peers[p.CleanID] == p }

// activePartnerLocked returns the partner of an active session between sender and targetID, checked from both
// sides. An empty targetID means the single session of a non-multi peer. Requires mu (read or write).
func (r *Relay) activePartnerLocked(sender *Peer, targetID string) *Peer {
	if !r.isCurrentLocked(sender) {
		return nil
	}
	if targetID == "" {
		targetID = sender.sessionWith
	}
	if _, active := sender.sessionLocked(targetID); !active {
		return nil
	}
	target := r.peers[targetID]
	if target == nil {
		return nil
	}
	if _, active := target.sessionLocked(sender.CleanID); !active {
		return nil
	}
	return target
}

// detachPartnersLocked ends every session of a leaving connection on the partner side and returns the
// partners that were in a session with it. Requires the write lock.
func (r *Relay) detachPartnersLocked(p *Peer) []*Peer {
	var out []*Peer
	if partner := r.peers[p.sessionWith]; partner != nil {
		if exists, _ := partner.sessionLocked(p.CleanID); exists {
			partner.endLocked(p.CleanID)
			out = append(out, partner)
		}
	}
	for hostID := range p.views {
		if host := r.peers[hostID]; host != nil && host.sessionWith == p.CleanID {
			host.sessionWith, host.sessionActive = "", false
			out = append(out, host)
		}
		p.endLocked(hostID)
	}
	return out
}

// sessionLocked reports whether p has a session (pending or active) with id, as host, single-session viewer
// or multi-session viewer. Requires Relay.mu.
func (p *Peer) sessionLocked(id string) (exists, active bool) {
	if id == "" {
		return false, false
	}
	if p.sessionWith == id {
		return true, p.sessionActive
	}
	if v := p.views[id]; v != nil {
		return true, v.active
	}
	return false, false
}

// busyLocked: a peer that hosts or views anything cannot be taken as a host. Requires Relay.mu.
func (p *Peer) busyLocked() bool { return p.sessionWith != "" || len(p.views) > 0 }

func (p *Peer) activateLocked(id string) {
	if p.sessionWith == id {
		p.sessionActive = true
	} else if v := p.views[id]; v != nil {
		v.active = true
	}
}

// endLocked removes p's session with id only; other views of a multi-session viewer are untouched.
func (p *Peer) endLocked(id string) {
	if p.sessionWith == id {
		p.sessionWith, p.sessionActive = "", false
		return
	}
	if v := p.views[id]; v != nil {
		v.stop()
		delete(p.views, id)
	}
}
