package usecase

import (
	"slices"

	"opendesk/server-relay/internal/model"
)

// Monitor wall: administrators watch up to model.MaxMonitorTiles hosts at once, read-only
// (see docs/features/monitor-wall.md at the repository root).
//
// A watch is not a session. ActivePartner, RouteFile and the session paths of ForwardBinary only know sessions,
// so a watch never authorizes input, clipboard, files, P2P signals or voice in either direction, and it neither
// counts toward MaxViewSessions nor makes a host busy. The administrator never addresses the host: only the
// relay tells it to start or stop its thumbnail stream. Each host streams once however many administrators
// watch it; the relay fans the frames out, one bounded queue per tile.

// maxMonitorFrameBytes bounds one thumbnail (a 640 px JPEG is typically 15-40 KiB).
const maxMonitorFrameBytes = 512 << 10

// monitorFeed is one host's thumbnail stream and the administrators watching it (guarded by Relay.mu).
type monitorFeed struct {
	// host is the connection told to stream; nil while the host is offline or cannot stream.
	host  *Peer
	tiles map[*Peer]*monitorTile
	// last is the newest frame, shown at once to a new watcher. Written only by host's read loop (under the read
	// lock) or under the write lock.
	last []byte
}

// monitorTile is one administrator's tile. It has its own queue and writer, so a slow administrator only
// drops its own frames and never holds more than two per tile.
type monitorTile struct {
	frames *frameQueue
	done   chan struct{}
}

// MonitorNotice is one monitor-state message for an administrator.
type MonitorNotice struct {
	Admin *Peer
	Host  string
	State model.MonitorState
}

// MonitorUpdate is what the caller must send after a monitor change, outside the relay lock.
type MonitorUpdate struct {
	Notices []MonitorNotice
	Start   []*Peer // hosts that must start streaming (monitor-start)
	Stop    []*Peer // hosts nobody watches any more (monitor-stop)
}

// startTile creates a tile whose writer tags every frame with monitorTag and the host's clean ID.
func (p *Peer) startTile(hostClean string) *monitorTile {
	t := &monitorTile{frames: newFrameQueue(p.metrics), done: make(chan struct{})}
	go p.writeFrames(t.frames, t.done, monitorTag, hostClean)
	return t
}

func (t *monitorTile) stop() {
	close(t.done)
	t.frames.drain()
}

// WatchMonitors replaces the set of hosts admin watches with targets (clean IDs; invalid IDs, the admin itself
// and entries past model.MaxMonitorTiles are ignored). Only administrators that advertised monitor-view may
// watch; anyone else gets ErrNotAdmin and, if it advertised monitor-view, a denied notice per target.
func (r *Relay) WatchMonitors(admin *Peer, targets []string) (MonitorUpdate, error) {
	want := make([]string, 0, min(len(targets), model.MaxMonitorTiles))
	for _, id := range targets {
		if len(want) == model.MaxMonitorTiles {
			break
		}
		if model.IsValidDeskID(id) && id != admin.CleanID && !slices.Contains(want, id) {
			want = append(want, id)
		}
	}
	var up MonitorUpdate
	if !admin.IsAdmin || !admin.MonitorView {
		r.metrics.Rejected()
		if admin.MonitorView {
			for _, id := range want {
				up.Notices = append(up.Notices, MonitorNotice{Admin: admin, Host: id, State: model.MonitorDenied})
			}
		}
		return up, ErrNotAdmin
	}
	r.mu.Lock()
	defer r.mu.Unlock()
	if !r.isCurrentLocked(admin) {
		return up, nil
	}
	for id := range admin.watching {
		if !slices.Contains(want, id) {
			r.unwatchLocked(admin, id, &up)
		}
	}
	for _, id := range want {
		if admin.watching[id] == nil {
			r.watchLocked(admin, id, &up)
		}
	}
	return up, nil
}

// MonitorAttach runs after host registered: administrators already watching its ID see it go live (or
// unsupported), and it is told to stream.
func (r *Relay) MonitorAttach(host *Peer) MonitorUpdate {
	var up MonitorUpdate
	r.mu.Lock()
	defer r.mu.Unlock()
	feed := r.monitors[host.CleanID]
	if feed == nil || feed.host == host || !r.isCurrentLocked(host) {
		return up
	}
	state := r.attachLocked(feed, host, &up)
	for admin := range feed.tiles {
		up.Notices = append(up.Notices, MonitorNotice{Admin: admin, Host: host.CleanID, State: state})
	}
	return up
}

// MonitorDetach runs when p's connection ends, in either role: its own tiles are removed (hosts nobody watches
// any more are stopped) and, if it was streaming, its watchers see it go offline. Their watches stay, so the
// tiles go live again when the host reconnects.
func (r *Relay) MonitorDetach(p *Peer) MonitorUpdate {
	var up MonitorUpdate
	r.mu.Lock()
	defer r.mu.Unlock()
	for id := range p.watching {
		r.unwatchLocked(p, id, &up)
	}
	if feed := r.monitors[p.CleanID]; feed != nil && feed.host == p {
		feed.host, feed.last = nil, nil
		for admin := range feed.tiles {
			up.Notices = append(up.Notices, MonitorNotice{Admin: admin, Host: p.CleanID, State: model.MonitorOffline})
		}
	}
	return up
}

func (r *Relay) watchLocked(admin *Peer, hostID string, up *MonitorUpdate) {
	feed := r.monitors[hostID]
	if feed == nil {
		feed = &monitorFeed{tiles: make(map[*Peer]*monitorTile, 1)}
		r.monitors[hostID] = feed
	}
	tile := admin.startTile(hostID)
	feed.tiles[admin] = tile
	if admin.watching == nil {
		admin.watching = make(map[string]*monitorTile, model.MaxMonitorTiles)
	}
	admin.watching[hostID] = tile
	state := r.attachLocked(feed, r.peers[hostID], up)
	up.Notices = append(up.Notices, MonitorNotice{Admin: admin, Host: hostID, State: state})
	if feed.last != nil {
		tile.frames.push(feed.last)
	}
}

func (r *Relay) unwatchLocked(admin *Peer, hostID string, up *MonitorUpdate) {
	if tile := admin.watching[hostID]; tile != nil {
		tile.stop()
	}
	delete(admin.watching, hostID)
	feed := r.monitors[hostID]
	if feed == nil {
		return
	}
	delete(feed.tiles, admin)
	if len(feed.tiles) == 0 {
		delete(r.monitors, hostID)
		if feed.host != nil && r.isCurrentLocked(feed.host) {
			up.Stop = append(up.Stop, feed.host)
		}
	}
}

// attachLocked makes host (nil when offline) the feed's streamer if it can stream, queues its monitor-start
// once, and returns the state its watchers see.
func (r *Relay) attachLocked(feed *monitorFeed, host *Peer, up *MonitorUpdate) model.MonitorState {
	switch {
	case host == nil:
		feed.host, feed.last = nil, nil
		return model.MonitorOffline
	case !host.MonitorHost:
		feed.host, feed.last = nil, nil
		return model.MonitorUnsupported
	}
	if feed.host != host {
		feed.host, feed.last = host, nil
		up.Start = append(up.Start, host)
	}
	return model.MonitorLive
}

// fanOutMonitor delivers one thumbnail from host to every administrator watching it. Only plain JPEGs are
// accepted, so every frame stands alone and a tile may drop any of them without a key-frame request.
// Holding the read lock while pushing keeps a tile from being stopped mid-push; pushes never block.
func (r *Relay) fanOutMonitor(host *Peer, jpeg []byte) bool {
	if !host.MonitorHost || len(jpeg) > maxMonitorFrameBytes || !isPlainJPEG(jpeg) {
		r.metrics.Rejected()
		return false
	}
	r.mu.RLock()
	defer r.mu.RUnlock()
	feed := r.monitors[host.CleanID]
	if feed == nil || feed.host != host {
		r.metrics.Rejected()
		return false
	}
	feed.last = jpeg
	for _, tile := range feed.tiles {
		tile.frames.push(jpeg)
	}
	return true
}

// WatchCount is how many hosts admin watches.
func (r *Relay) WatchCount(admin *Peer) int {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return len(admin.watching)
}

func isPlainJPEG(b []byte) bool { return len(b) > 3 && b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF }
