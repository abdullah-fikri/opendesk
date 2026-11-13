package usecase

import (
	"crypto/rand"
	"crypto/subtle"
	"encoding/hex"
	"time"
)

// DefaultResumeGrace is how long the sessions of a lost session-resume connection wait for it (see
// docs/features/session-resume.md at the repository root).
const DefaultResumeGrace = 30 * time.Second

// newResumeToken returns 32 random hex digits.
func newResumeToken() string {
	var raw [16]byte
	_, _ = rand.Read(raw[:])
	return hex.EncodeToString(raw[:])
}

// canResume reports whether next may take over old's sessions: both list session-resume, next presents old's
// token, both agree on multi-session, and old is still connected or suspended within its grace.
func canResume(old, next *Peer, token string, now time.Time) bool {
	return old.Resume && next.Resume && token != "" && old.ResumeToken != "" &&
		subtle.ConstantTimeCompare([]byte(token), []byte(old.ResumeToken)) == 1 &&
		old.Multi == next.Multi && (old.suspendedUntil.IsZero() || now.Before(old.suspendedUntil))
}

// quietLocked: nothing is forwarded to or from a suspended or held peer. Requires Relay.mu.
func (p *Peer) quietLocked() bool { return !p.suspendedUntil.IsZero() || p.held }

// resumableLocked reports whether p's session with id may wait for a resume: both sides list session-resume
// and hold it active. Requires Relay.mu.
func (r *Relay) resumableLocked(p *Peer, id string) bool {
	partner := r.peers[id]
	if !p.Resume || partner == nil || partner == p || !partner.Resume {
		return false
	}
	_, mine := p.sessionLocked(id)
	_, theirs := partner.sessionLocked(p.CleanID)
	return mine && theirs
}

// splitLocked ends every session of p that cannot wait for a resume (on both sides) and returns the IDs of the
// sessions that can, plus the partners whose session ended. Requires the write lock.
func (r *Relay) splitLocked(p *Peer) (keep []string, closed []*Peer) {
	ids := make([]string, 0, 1+len(p.views))
	if p.sessionWith != "" {
		ids = append(ids, p.sessionWith)
	}
	for id := range p.views {
		ids = append(ids, id)
	}
	for _, id := range ids {
		if r.resumableLocked(p, id) {
			keep = append(keep, id)
			continue
		}
		if partner := r.peers[id]; partner != nil {
			if exists, _ := partner.sessionLocked(p.CleanID); exists {
				partner.endLocked(p.CleanID)
				closed = append(closed, partner)
			}
		}
		p.endLocked(id)
	}
	return keep, closed
}

// takeOverLocked moves old's resumable sessions to next, which stays held until Release; every other session
// of old ends and its partner lands in res.Orphans. Requires the write lock.
func (r *Relay) takeOverLocked(old, next *Peer, res *RegisterResult) {
	keep, closed := r.splitLocked(old)
	res.Orphans = closed
	for _, id := range keep {
		if next.Multi {
			if next.views == nil {
				next.views = make(map[string]*viewSession, len(keep))
			}
			next.views[id] = next.startView(id, true)
		} else {
			next.sessionWith, next.sessionActive = id, true
		}
		old.endLocked(id)
		res.Resumed = append(res.Resumed, id)
		if partner := r.peers[id]; !partner.suspendedUntil.IsZero() {
			res.Waiting = append(res.Waiting, id)
		} else {
			res.Back = append(res.Back, partner)
		}
	}
	next.held = len(keep) > 0
}

// Release lets traffic flow to and from a resumed connection once its registered-ack is queued.
func (r *Relay) Release(p *Peer) {
	r.mu.Lock()
	p.held = false
	r.mu.Unlock()
}

// Reconnecting reports whether the device cleanID lost its connection and its sessions wait for it.
func (r *Relay) Reconnecting(cleanID string) bool {
	r.mu.RLock()
	defer r.mu.RUnlock()
	p := r.peers[cleanID]
	return p != nil && !p.suspendedUntil.IsZero()
}

// Expired is a suspended connection that went offline; each partner must receive session-closed.
type Expired struct {
	Peer     *Peer
	Partners []*Peer
}

// ExpireSuspended ends every suspension whose grace is over at now, and every suspension left without a
// session (its partners left).
func (r *Relay) ExpireSuspended(now time.Time) []Expired {
	r.mu.Lock()
	var out []Expired
	for _, p := range r.peers {
		if p.suspendedUntil.IsZero() || now.Before(p.suspendedUntil) && p.busyLocked() {
			continue
		}
		out = append(out, r.expireLocked(p))
	}
	r.mu.Unlock()
	for _, e := range out {
		r.markGone(e.Peer.CleanID)
	}
	return out
}

// EndSuspension ends p's suspension at once (the device is no longer admitted). It returns false when p is not
// a current suspended connection.
func (r *Relay) EndSuspension(p *Peer) (Expired, bool) {
	r.mu.Lock()
	if !r.isCurrentLocked(p) || p.suspendedUntil.IsZero() {
		r.mu.Unlock()
		return Expired{}, false
	}
	e := r.expireLocked(p)
	r.mu.Unlock()
	r.markGone(p.CleanID)
	return e, true
}

func (r *Relay) expireLocked(p *Peer) Expired {
	e := Expired{Peer: p, Partners: r.detachPartnersLocked(p)}
	delete(r.peers, p.CleanID)
	r.metrics.ObservePeers(len(r.peers))
	return e
}
