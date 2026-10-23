package gateway

import (
	"crypto/subtle"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"sync"
	"sync/atomic"
	"time"

	"opendesk/server-relay/internal/database"
	"opendesk/server-relay/internal/logger"

	"github.com/gorilla/websocket"
)

type deviceCredential struct {
	Token string `json:"token"`
	Admin bool   `json:"admin"`
}

type Gateway struct {
	credentials map[string]deviceCredential
	peers       map[string]*Peer
	fileGrants  map[string]*fileGrant // guarded by peersMu
	peersMu     sync.RWMutex
	db          *database.Store
	upgrader    websocket.Upgrader
	start       time.Time
	heartbeat   atomic.Int64 // nanoseconds; 0 disables liveness deadlines
}

// fileGrant lets one transfer finish after its screen session ends, but only between the same two connections.
type fileGrant struct {
	from, to *Peer
	expires  time.Time
}

const (
	fileGrantIdle    = 60 * time.Second
	maxGrantsPerPeer = 8
	maxMessageBytes  = 16 << 20
	maxCaps          = 16
)

// serverCaps is advertised in registered-ack so clients can detect relay features.
// OPENDESK_P2P_DISABLED=1 withdraws "p2p-signal": clients then keep every session on the relay.
func serverCaps() []string {
	if os.Getenv("OPENDESK_P2P_DISABLED") == "1" {
		return []string{"file-grant", "file-offer", "caps"}
	}
	return []string{"file-grant", "file-offer", "p2p-signal", "caps"}
}

func NewGateway(readBuf, writeBuf int, db *database.Store) *Gateway {
	credentials := make(map[string]deviceCredential)
	if err := json.Unmarshal([]byte(os.Getenv("OPENDESK_DEVICE_CREDENTIALS")), &credentials); err != nil && os.Getenv("OPENDESK_DEVICE_CREDENTIALS") != "" {
		logger.Error("main", "Invalid OPENDESK_DEVICE_CREDENTIALS; refusing all registrations: %v", err)
	}
	return &Gateway{
		credentials: credentials,
		peers:       make(map[string]*Peer),
		fileGrants:  make(map[string]*fileGrant),
		db:          db,
		start:       time.Now(),
		upgrader: websocket.Upgrader{
			ReadBufferSize:  readBuf,
			WriteBufferSize: writeBuf,
			// Shared write buffers: an idle or slow peer holds none, so memory follows active traffic only.
			WriteBufferPool:   &sync.Pool{},
			EnableCompression: false,
			CheckOrigin: func(r *http.Request) bool {
				return true
			},
		},
	}
}

// authenticate admits every device when no credentials are configured (the default). Setting
// OPENDESK_DEVICE_CREDENTIALS turns on per-device tokens; then only the relay's admin flag counts.
func (g *Gateway) authenticate(clean, token string, claimedAdmin bool) (bool, bool) {
	if len(g.credentials) == 0 {
		return true, claimedAdmin
	}
	credential, ok := g.credentials[clean]
	if !ok || len(credential.Token) < 32 || len(token) != len(credential.Token) || subtle.ConstantTimeCompare([]byte(token), []byte(credential.Token)) != 1 {
		return false, false
	}
	return true, credential.Admin
}

func (g *Gateway) broadcastHostListToAdmins() {
	if g.db == nil {
		return
	}
	go func() {
		g.peersMu.RLock()
		admins := make([]*Peer, 0)
		for _, p := range g.peers {
			if p.IsAdmin {
				admins = append(admins, p)
			}
		}
		g.peersMu.RUnlock()

		if len(admins) == 0 {
			return
		}
		hostsList := g.hostList()
		payload := map[string]interface{}{
			"type":  "host-list",
			"hosts": hostsList,
		}
		for _, adm := range admins {
			_ = adm.SendJSON(payload)
		}
	}()
}

func (g *Gateway) hostList() interface{} {
	started := time.Now()
	hosts := g.db.GetAllHosts()
	metrics.observeHostList(time.Since(started))
	return hosts
}

func (g *Gateway) getPeer(id string) *Peer {
	g.peersMu.RLock()
	defer g.peersMu.RUnlock()
	return g.peers[id]
}

func (g *Gateway) sessionPeer(sender *Peer, targetID string) *Peer {
	g.peersMu.RLock()
	defer g.peersMu.RUnlock()
	return g.activePartnerLocked(sender, targetID)
}

// activePartnerLocked requires peersMu (read or write).
func (g *Gateway) activePartnerLocked(sender *Peer, targetID string) *Peer {
	if sender == nil || g.peers[sender.CleanID] != sender || !sender.SessionActive {
		return nil
	}
	if targetID == "" {
		targetID = sender.SessionWith
	}
	if sender.SessionWith != targetID {
		return nil
	}
	target := g.peers[targetID]
	if target == nil || !target.SessionActive || target.SessionWith != sender.CleanID {
		return nil
	}
	return target
}

func grantKey(fromClean, id string) string { return fromClean + ":" + id }

// routeFile returns the receiver of a relay-file payload. "start" needs an active session and creates a grant;
// "chunk"/"end" need that grant; "ack" flows back from receiver to sender and consumes it.
func (g *Gateway) routeFile(sender *Peer, targetID, action, id string) *Peer {
	if sender == nil || id == "" || len(id) > 80 {
		return nil
	}
	now := time.Now()
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if g.peers[sender.CleanID] != sender {
		return nil
	}
	switch action {
	case "start":
		target := g.activePartnerLocked(sender, targetID)
		key := grantKey(sender.CleanID, id)
		if target == nil || g.fileGrants[key] != nil {
			return nil
		}
		owned := 0
		for _, gr := range g.fileGrants {
			if gr.from == sender {
				owned++
			}
		}
		if owned >= maxGrantsPerPeer {
			return nil
		}
		g.fileGrants[key] = &fileGrant{from: sender, to: target, expires: now.Add(fileGrantIdle)}
		return target
	case "offer", "pull":
		// Clipboard file offers carry only names/sizes; a pull asks the offerer to start a normal granted transfer.
		return g.activePartnerLocked(sender, targetID)
	case "chunk", "end":
		gr := g.fileGrants[grantKey(sender.CleanID, id)]
		if gr == nil || gr.from != sender || g.peers[gr.to.CleanID] != gr.to || now.After(gr.expires) || (targetID != "" && targetID != gr.to.CleanID) {
			return nil
		}
		gr.expires = now.Add(fileGrantIdle)
		return gr.to
	case "ack":
		key := grantKey(targetID, id)
		gr := g.fileGrants[key]
		if gr == nil || gr.to != sender || g.peers[gr.from.CleanID] != gr.from || now.After(gr.expires) {
			return nil
		}
		delete(g.fileGrants, key)
		return gr.from
	}
	return nil
}

// dropGrantsLocked requires the peersMu write lock.
func (g *Gateway) dropGrantsLocked(p *Peer, now time.Time) {
	for key, gr := range g.fileGrants {
		if gr.from == p || gr.to == p || now.After(gr.expires) {
			delete(g.fileGrants, key)
		}
	}
}

func sanitizeCaps(raw interface{}) []string {
	list, _ := raw.([]interface{})
	caps := make([]string, 0, len(list))
	for _, item := range list {
		s, ok := item.(string)
		if !ok || len(s) == 0 || len(s) > 32 || len(caps) >= maxCaps {
			continue
		}
		valid := true
		for _, c := range s {
			if !(c >= 'a' && c <= 'z' || c >= '0' && c <= '9' || c == '-' || c == '.') {
				valid = false
				break
			}
		}
		if valid {
			caps = append(caps, s)
		}
	}
	return caps
}

func (g *Gateway) setSession(caller, target *Peer, active bool) bool {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if caller == nil || target == nil || g.peers[caller.CleanID] != caller || g.peers[target.CleanID] != target || caller.SessionWith != "" || target.SessionWith != "" {
		return false
	}
	caller.SessionWith, target.SessionWith = target.CleanID, caller.CleanID
	caller.SessionActive, target.SessionActive = active, active
	return true
}

func (g *Gateway) approveSession(caller, target *Peer) bool {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if caller == nil || target == nil || g.peers[caller.CleanID] != caller || g.peers[target.CleanID] != target || caller.SessionWith != target.CleanID || target.SessionWith != caller.CleanID || caller.SessionActive || target.SessionActive {
		return false
	}
	caller.SessionActive, target.SessionActive = true, true
	return true
}

func (g *Gateway) closePendingSession(sender *Peer, targetID string) *Peer {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if sender == nil || g.peers[sender.CleanID] != sender || sender.SessionActive || sender.SessionWith != targetID {
		return nil
	}
	target := g.peers[targetID]
	if target == nil || target.SessionActive || target.SessionWith != sender.CleanID {
		return nil
	}
	target.SessionWith, sender.SessionWith = "", ""
	return target
}

func (g *Gateway) closeSession(sender *Peer, targetID string) *Peer {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if sender == nil || g.peers[sender.CleanID] != sender || sender.SessionWith != targetID {
		return nil
	}
	target := g.peers[targetID]
	if target != nil && target.SessionWith == sender.CleanID {
		target.SessionWith, target.SessionActive = "", false
	}
	sender.SessionWith, sender.SessionActive = "", false
	return target
}

// registerPeer returns the session partner of a replaced connection; the caller must notify it outside the lock.
func (g *Gateway) registerPeer(p *Peer) (orphan *Peer) {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if old, ok := g.peers[p.CleanID]; ok && old != p {
		if partner := g.peers[old.SessionWith]; partner != nil && partner.SessionWith == old.CleanID {
			partner.SessionWith, partner.SessionActive = "", false
			orphan = partner
		}
		g.dropGrantsLocked(old, time.Now())
		old.Close()
		_ = old.WS.Close()
	}
	g.peers[p.CleanID] = p
	metrics.observePeers(len(g.peers))
	return orphan
}

func (g *Gateway) removePeer(cleanID string, expected *Peer) *Peer {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if p, ok := g.peers[cleanID]; ok {
		if expected != nil && p != expected {
			return nil
		}
		p.Close()
		if partner := g.peers[p.SessionWith]; partner != nil && partner.SessionWith == p.CleanID {
			partner.SessionWith, partner.SessionActive = "", false
		}
		g.dropGrantsLocked(p, time.Now())
		delete(g.peers, cleanID)
		return p
	}
	return nil
}

func (g *Gateway) countPeers() int {
	g.peersMu.RLock()
	defer g.peersMu.RUnlock()
	return len(g.peers)
}

func (g *Gateway) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if websocket.IsWebSocketUpgrade(r) {
		g.handleWebSocket(w, r)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	if r.URL.Path == "/metrics" {
		// Loopback is not proof of trust behind a local TLS terminator, so metrics need their own token.
		token := os.Getenv("OPENDESK_METRICS_TOKEN")
		given := r.Header.Get("Authorization")
		if len(token) < 32 || subtle.ConstantTimeCompare([]byte(given), []byte("Bearer "+token)) != 1 {
			http.Error(w, `{"error":"unauthorized"}`, http.StatusUnauthorized)
			return
		}
		_ = json.NewEncoder(w).Encode(metrics.snapshot(g.countPeers()))
		return
	}
	w.Header().Set("Access-Control-Allow-Origin", "*")
	_ = json.NewEncoder(w).Encode(map[string]interface{}{
		"service":     "opendesk.relay",
		"version":     "2.5.0",
		"status":      "online",
		"storage":     "postgresql",
		"activePeers": g.countPeers(),
		"uptime":      int(time.Since(g.start).Seconds()),
	})
}

func (g *Gateway) handleWebSocket(w http.ResponseWriter, r *http.Request) {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	// Behind a TLS terminator in another network namespace (cloudflared -> Docker) peers arrive from the bridge IP,
	// not loopback; OPENDESK_TLS_PROXY=1 trusts that hop, so publish the port on 127.0.0.1 only.
	if err != nil || (r.TLS == nil && !net.ParseIP(host).IsLoopback() && os.Getenv("OPENDESK_TLS_PROXY") != "1") {
		http.Error(w, "WSS required outside loopback", http.StatusForbidden)
		return
	}
	conn, err := g.upgrader.Upgrade(w, r, nil)
	if err != nil {
		logger.Error("anynet.ws_acceptor", "WebSocket upgrade failed from %s: %v", r.RemoteAddr, err)
		return
	}
	defer conn.Close()
	// Largest legitimate message is a native-resolution key frame or a file chunk; anything bigger is abuse.
	conn.SetReadLimit(maxMessageBytes)

	var registeredID string
	var currentPeer *Peer
	var lastSignal time.Time
	// After registration other goroutines write through currentPeer; raw conn writes would race with them.
	reply := func(v interface{}) {
		if currentPeer != nil {
			_ = currentPeer.SendJSON(v)
		} else {
			_ = conn.WriteJSON(v)
		}
	}
	// Native Ping/Pong liveness runs beside the legacy JSON heartbeat; RFC 6455 clients answer pings automatically.
	liveness := 4 * time.Duration(g.heartbeat.Load())
	if liveness > 0 {
		_ = conn.SetReadDeadline(time.Now().Add(liveness))
		conn.SetPongHandler(func(string) error { return conn.SetReadDeadline(time.Now().Add(liveness)) })
	}

	defer func() {
		if registeredID != "" {
			p := g.removePeer(registeredID, currentPeer)
			if p != nil {
				if g.db != nil {
					g.db.SetHostOffline(registeredID)
					g.db.CloseSessionsForPeer(registeredID)
				}
				g.broadcastHostListToAdmins()

				partnerID := p.SessionWith
				if partnerID != "" {
					partner := g.getPeer(partnerID)
					if partner != nil {
						_ = partner.SendJSON(map[string]interface{}{
							"type":    "session-closed",
							"message": "Perangkat rekan terputus / offline.",
						})
					}
				}
				logger.Info("anynet.connection_mgr", "Client disconnected: %s (active_peers=%d)", FormatDeskID(registeredID), g.countPeers())
			}
		}
	}()

	for {
		msgType, message, err := conn.ReadMessage()
		if err != nil {
			break
		}
		if liveness > 0 {
			_ = conn.SetReadDeadline(time.Now().Add(liveness))
		}

		// Zero-copy binary relay between active session partners (0x02 = Voice Audio, 0xFF 0xD8 = Video JPEG)
		if msgType == websocket.BinaryMessage {
			if len(message) > 0 && message[0] == 0x02 {
				metrics.countIngress("audio-bin", len(message))
			} else {
				metrics.countIngress("video-bin", len(message))
			}
			if currentPeer != nil {
				if rcpt := g.sessionPeer(currentPeer, ""); rcpt != nil {
					if len(message) > 1 && message[0] == 0x02 {
						_ = rcpt.SendRawBinary(message)
					} else if len(message) > 2 && message[0] == 0xff && message[1] == 0xd8 {
						rcpt.EnqueueBinaryFrame(message)
					}
				} else {
					metrics.rejected.Add(1)
				}
			}
			continue
		}

		var raw map[string]interface{}
		if err := json.Unmarshal(message, &raw); err != nil {
			continue
		}

		t, _ := raw["type"].(string)
		metrics.countIngress(ingressKind(t), len(message))
		if t != "register" && (currentPeer == nil || g.getPeer(currentPeer.CleanID) != currentPeer) {
			continue
		}

		switch t {
		case "register":
			if currentPeer != nil {
				continue
			}
			deskID, _ := raw["deskId"].(string)
			clean := CleanDeskID(deskID)
			if len(clean) != 9 || clean[0] == '0' {
				_ = conn.WriteJSON(map[string]string{"type": "error", "message": "Desk ID tidak valid."})
				continue
			}
			token, _ := raw["deviceToken"].(string)
			claimedAdmin, _ := raw["isAdmin"].(bool)
			allowed, isAdmin := g.authenticate(clean, token, claimedAdmin)
			if !allowed {
				// Unprovisioned devices are the usual "it connects but nothing happens" cause; name them for the operator.
				logger.Warn("anynet.any_socket", "Registration rejected (not in OPENDESK_DEVICE_CREDENTIALS or wrong token): id=%s addr=%s", FormatDeskID(clean), r.RemoteAddr)
				_ = conn.WriteJSON(map[string]string{"type": "error", "message": "Kredensial perangkat tidak valid."})
				return
			}
			registeredID = clean
			platform, _ := raw["platform"].(string)
			if platform == "" {
				platform = "device"
			}
			devName, _ := raw["deviceName"].(string)
			if devName == "" {
				devName = "OpenDesk Client"
			}
			peer := NewPeer(clean, platform, devName, isAdmin, conn)
			peer.Caps = sanitizeCaps(raw["caps"])
			currentPeer = peer
			if orphan := g.registerPeer(peer); orphan != nil {
				_ = orphan.SendJSON(map[string]string{
					"type":    "session-closed",
					"message": "Perangkat rekan tersambung ulang.",
				})
			}
			if g.db != nil {
				g.db.UpsertHost(clean, peer.DeskID, devName, platform, r.RemoteAddr, isAdmin, true)
			}

			logger.Info("anynet.any_socket", "Client registered: id=%s name=%s platform=%s admin=%v addr=%s (active_peers=%d)", peer.DeskID, devName, platform, isAdmin, r.RemoteAddr, g.countPeers())
			_ = peer.SendJSON(map[string]interface{}{
				"type":   "registered-ack",
				"deskId": peer.DeskID,
				"caps":   serverCaps(),
			})
			g.broadcastHostListToAdmins()

		case "get-hosts":
			caller := g.getPeer(registeredID)
			if caller != nil && caller.IsAdmin && g.db != nil {
				_ = caller.SendJSON(map[string]interface{}{
					"type":  "host-list",
					"hosts": g.hostList(),
				})
			}

		case "connect-request":
			targetID, _ := raw["targetId"].(string)
			targetClean := CleanDeskID(targetID)
			if targetClean == "" {
				reply(map[string]string{"type": "error", "message": "Masukkan ID target."})
				continue
			}
			if registeredID != "" && targetClean == registeredID {
				reply(map[string]string{"type": "error", "message": "Tidak dapat me-remote perangkat sendiri."})
				continue
			}

			targetPeer := g.getPeer(targetClean)
			callerPeer := g.getPeer(registeredID)
			if targetPeer != nil && callerPeer != nil && g.setSession(callerPeer, targetPeer, targetPeer.Platform != "android") {

				if targetPeer.Platform == "android" {
					if g.db != nil {
						g.db.StartSession(registeredID, callerPeer.DeskID, targetClean, targetPeer.DeskID, "pending")
					}
					_ = targetPeer.SendJSON(map[string]interface{}{
						"type":       "incoming-request",
						"fromId":     callerPeer.DeskID,
						"fromClean":  registeredID,
						"deviceName": callerPeer.DeviceName,
					})
				} else {
					if g.db != nil {
						g.db.StartSession(registeredID, callerPeer.DeskID, targetClean, targetPeer.DeskID, "active")
					}
					_ = targetPeer.SendJSON(map[string]interface{}{
						"type":        "session-connected",
						"targetId":    callerPeer.DeskID,
						"targetClean": registeredID,
						"role":        "host",
						"peerCaps":    callerPeer.Caps,
					})

					_ = callerPeer.SendJSON(map[string]interface{}{
						"type":        "session-connected",
						"targetId":    targetPeer.DeskID,
						"targetClean": targetClean,
						"role":        "viewer",
						"peerCaps":    targetPeer.Caps,
					})
				}

				logger.Info("app.session", "Unattended session granted: viewer=%s <-> host=%s (platform=%s)", callerPeer.DeskID, targetPeer.DeskID, targetPeer.Platform)
			} else {
				logger.Warn("anynet.any_socket", "Connect request failed: target=%s unavailable", FormatDeskID(targetClean))
				reply(map[string]string{
					"type":    "error",
					"message": fmt.Sprintf("Perangkat %s tidak ditemukan / offline.", FormatDeskID(targetClean)),
				})
			}

		case "approval-response":
			fromClean, _ := raw["fromClean"].(string)
			callerClean := CleanDeskID(fromClean)
			caller := g.getPeer(callerClean)
			target := g.getPeer(registeredID)
			accept, _ := raw["accept"].(bool)

			if caller != nil && target != nil {
				if accept {
					if !g.approveSession(caller, target) {
						continue
					}

					targetDesk := FormatDeskID(registeredID)
					if target != nil {
						targetDesk = target.DeskID
					}

					if g.db != nil {
						g.db.UpdateSessionStatus(callerClean, registeredID, "active")
					}

					_ = caller.SendJSON(map[string]interface{}{
						"type":        "session-connected",
						"targetId":    targetDesk,
						"targetClean": registeredID,
						"role":        "viewer",
						"peerCaps":    target.Caps,
					})

					_ = target.SendJSON(map[string]interface{}{
						"type":        "session-connected",
						"targetId":    caller.DeskID,
						"targetClean": callerClean,
						"role":        "host",
						"peerCaps":    caller.Caps,
					})

					logger.Info("app.session", "Session established: viewer=%s <-> host=%s", caller.DeskID, targetDesk)
				} else {
					if g.closePendingSession(target, callerClean) == nil {
						continue
					}
					if g.db != nil {
						g.db.UpdateSessionStatus(callerClean, registeredID, "rejected")
					}
					_ = caller.SendJSON(map[string]string{
						"type":    "error",
						"message": "Permintaan koneksi ditolak oleh perangkat target.",
					})
				}
			}

		case "relay-frame":
			targetClean, _ := raw["targetClean"].(string)
			rcpt := g.sessionPeer(currentPeer, CleanDeskID(targetClean))
			if rcpt != nil {
				rcpt.EnqueueJSONFrame(map[string]interface{}{
					"type":   "frame",
					"data":   raw["data"],
					"width":  raw["width"],
					"height": raw["height"],
				})
			}

		case "relay-input":
			targetClean, _ := raw["targetClean"].(string)
			rcpt := g.sessionPeer(currentPeer, CleanDeskID(targetClean))
			if rcpt != nil {
				_ = rcpt.SendJSON(map[string]interface{}{
					"type":  "input-command",
					"event": raw["event"],
				})
			}

		case "relay-clipboard":
			targetClean, _ := raw["targetClean"].(string)
			rcpt := g.sessionPeer(currentPeer, CleanDeskID(targetClean))
			if rcpt != nil {
				_ = rcpt.SendJSON(map[string]interface{}{
					"type": "clipboard-sync",
					"text": raw["text"],
				})
			}

		case "relay-file":
			targetClean, _ := raw["targetClean"].(string)
			payload, _ := raw["payload"].(map[string]interface{})
			action, _ := payload["action"].(string)
			id, _ := payload["id"].(string)
			// Transfers start only inside an approved session; the grant lets them finish after it ends.
			if rcpt := g.routeFile(currentPeer, CleanDeskID(targetClean), action, id); rcpt != nil {
				_ = rcpt.SendJSON(map[string]interface{}{
					"type":      "file-transfer",
					"fromClean": registeredID,
					"payload":   payload,
				})
			} else {
				metrics.rejected.Add(1)
			}

		case "session-disconnect":
			targetClean, _ := raw["targetClean"].(string)
			cleanTarget := CleanDeskID(targetClean)
			rcpt := g.closeSession(currentPeer, cleanTarget)
			if rcpt == nil {
				continue
			}
			if g.db != nil {
				g.db.CloseSessionsForPeer(registeredID)
			}
			_ = rcpt.SendJSON(map[string]string{
				"type":    "session-closed",
				"message": "Sesi diakhiri oleh rekan koneksi.",
			})
			logger.Info("app.session", "Session terminated by client: id=%s", FormatDeskID(registeredID))

		case "p2p-signal":
			if os.Getenv("OPENDESK_P2P_DISABLED") == "1" || len(message) > 4096 || time.Since(lastSignal) < 100*time.Millisecond {
				continue
			}
			targetID, _ := raw["targetClean"].(string)
			candidate, ok := raw["candidate"].(string)
			if !ok || len(candidate) > 2048 || len(candidate) == 0 {
				continue
			}
			if rcpt := g.sessionPeer(currentPeer, CleanDeskID(targetID)); rcpt != nil {
				lastSignal = time.Now()
				_ = rcpt.SendJSON(map[string]string{"type": "p2p-signal", "fromClean": registeredID, "candidate": candidate})
			}

		case "ping":
			timeVal := raw["time"]
			_ = currentPeer.SendJSON(map[string]interface{}{
				"type": "pong",
				"time": timeVal,
			})
		}
	}
}

func (g *Gateway) StartHeartbeat(interval time.Duration) {
	g.heartbeat.Store(int64(interval))
	ticker := time.NewTicker(interval)
	go func() {
		for range ticker.C {
			g.peersMu.Lock()
			peersList := make([]*Peer, 0, len(g.peers))
			for _, p := range g.peers {
				peersList = append(peersList, p)
			}
			g.dropGrantsLocked(nil, time.Now())
			g.peersMu.Unlock()

			now := time.Now().UnixMilli()
			for _, p := range peersList {
				_ = p.SendPing()
				// Legacy JSON heartbeat stays until every deployed client is known to answer native pings.
				_ = p.SendJSON(map[string]interface{}{
					"type": "heartbeat",
					"time": now,
				})
			}
		}
	}()
}
