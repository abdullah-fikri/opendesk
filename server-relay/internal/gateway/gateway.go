package gateway

import (
	"encoding/json"
	"fmt"
	"net/http"
	"sync"
	"time"

	"opendesk/server-relay/internal/database"
	"opendesk/server-relay/internal/logger"

	"github.com/gorilla/websocket"
)

type Gateway struct {
	peers    map[string]*Peer
	peersMu  sync.RWMutex
	db       *database.Store
	upgrader websocket.Upgrader
	start    time.Time
}

func NewGateway(readBuf, writeBuf int, db *database.Store) *Gateway {
	return &Gateway{
		peers: make(map[string]*Peer),
		db:    db,
		start: time.Now(),
		upgrader: websocket.Upgrader{
			ReadBufferSize:    readBuf,
			WriteBufferSize:   writeBuf,
			EnableCompression: false,
			CheckOrigin: func(r *http.Request) bool {
				return true
			},
		},
	}
}

func (g *Gateway) broadcastHostListToAdmins() {
	if g.db == nil {
		return
	}
	go func() {
		hostsList := g.db.GetAllHosts()
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
		payload := map[string]interface{}{
			"type":  "host-list",
			"hosts": hostsList,
		}
		for _, adm := range admins {
			_ = adm.SendJSON(payload)
		}
	}()
}

func (g *Gateway) getPeer(id string) *Peer {
	g.peersMu.RLock()
	defer g.peersMu.RUnlock()
	return g.peers[id]
}

func (g *Gateway) registerPeer(p *Peer) {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if old, ok := g.peers[p.CleanID]; ok && old != p {
		old.Close()
		_ = old.WS.Close()
	}
	g.peers[p.CleanID] = p
}

func (g *Gateway) removePeer(cleanID string, expected *Peer) *Peer {
	g.peersMu.Lock()
	defer g.peersMu.Unlock()
	if p, ok := g.peers[cleanID]; ok {
		if expected != nil && p != expected {
			return nil
		}
		p.Close()
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
	conn, err := g.upgrader.Upgrade(w, r, nil)
	if err != nil {
		logger.Error("anynet.ws_acceptor", "WebSocket upgrade failed from %s: %v", r.RemoteAddr, err)
		return
	}
	defer conn.Close()

	var registeredID string
	var currentPeer *Peer

	defer func() {
		if registeredID != "" {
			p := g.removePeer(registeredID, currentPeer)
			if p != nil {
				var dbPartner string
				if g.db != nil {
					g.db.SetHostOffline(registeredID)
					dbPartner = g.db.CloseSessionsForPeer(registeredID)
				}
				g.broadcastHostListToAdmins()

				partnerID := p.SessionWith
				if partnerID == "" {
					partnerID = dbPartner
				}
				if partnerID != "" {
					partner := g.getPeer(partnerID)
					if partner != nil {
						partner.SessionWith = ""
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

		// Zero-copy binary relay between active session partners (0x02 = Voice Audio, 0xFF 0xD8 = Video JPEG)
		if msgType == websocket.BinaryMessage {
			if registeredID != "" {
				if sender := g.getPeer(registeredID); sender != nil && sender.SessionWith != "" {
					if rcpt := g.getPeer(sender.SessionWith); rcpt != nil {
						if len(message) > 1 && message[0] == 0x02 {
							_ = rcpt.SendRawBinary(message)
						} else {
							rcpt.EnqueueBinaryFrame(message)
						}
					}
				}
			}
			continue
		}

		var raw map[string]interface{}
		if err := json.Unmarshal(message, &raw); err != nil {
			continue
		}

		t, _ := raw["type"].(string)

		switch t {
		case "register":
			deskID, _ := raw["deskId"].(string)
			clean := CleanDeskID(deskID)
			if clean == "" {
				_ = conn.WriteJSON(map[string]string{"type": "error", "message": "Desk ID tidak valid."})
				continue
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
			isAdmin, _ := raw["isAdmin"].(bool)

			peer := NewPeer(clean, platform, devName, isAdmin, conn)
			currentPeer = peer
			g.registerPeer(peer)
			if g.db != nil {
				g.db.UpsertHost(clean, peer.DeskID, devName, platform, r.RemoteAddr, isAdmin, true)
			}

			logger.Info("anynet.any_socket", "Client registered: id=%s name=%s platform=%s admin=%v addr=%s (active_peers=%d)", peer.DeskID, devName, platform, isAdmin, r.RemoteAddr, g.countPeers())
			_ = peer.SendJSON(map[string]string{
				"type":   "registered-ack",
				"deskId": peer.DeskID,
			})
			g.broadcastHostListToAdmins()

		case "get-hosts":
			caller := g.getPeer(registeredID)
			if caller != nil && caller.IsAdmin && g.db != nil {
				_ = caller.SendJSON(map[string]interface{}{
					"type":  "host-list",
					"hosts": g.db.GetAllHosts(),
				})
			}

		case "connect-request":
			targetID, _ := raw["targetId"].(string)
			targetClean := CleanDeskID(targetID)
			if targetClean == "" {
				_ = conn.WriteJSON(map[string]string{"type": "error", "message": "Masukkan ID target."})
				continue
			}
			if registeredID != "" && targetClean == registeredID {
				_ = conn.WriteJSON(map[string]string{"type": "error", "message": "Tidak dapat me-remote perangkat sendiri."})
				continue
			}

			targetPeer := g.getPeer(targetClean)
			callerPeer := g.getPeer(registeredID)
			if targetPeer != nil && callerPeer != nil {
				targetPeer.SessionWith = registeredID
				callerPeer.SessionWith = targetClean

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
					})

					_ = callerPeer.SendJSON(map[string]interface{}{
						"type":        "session-connected",
						"targetId":    targetPeer.DeskID,
						"targetClean": targetClean,
						"role":        "viewer",
					})
				}

				logger.Info("app.session", "Unattended session granted: viewer=%s <-> host=%s (platform=%s)", callerPeer.DeskID, targetPeer.DeskID, targetPeer.Platform)
			} else {
				logger.Warn("anynet.any_socket", "Connect request failed: target=%s offline", FormatDeskID(targetClean))
				_ = conn.WriteJSON(map[string]string{
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

			if caller != nil {
				if accept {
					if target != nil {
						target.SessionWith = callerClean
					}
					caller.SessionWith = registeredID

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
					})

					_ = conn.WriteJSON(map[string]interface{}{
						"type":        "session-connected",
						"targetId":    caller.DeskID,
						"targetClean": callerClean,
						"role":        "host",
					})

					logger.Info("app.session", "Session established: viewer=%s <-> host=%s", caller.DeskID, targetDesk)
				} else {
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
			rcpt := g.getPeer(CleanDeskID(targetClean))
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
			rcpt := g.getPeer(CleanDeskID(targetClean))
			if rcpt != nil {
				_ = rcpt.SendJSON(map[string]interface{}{
					"type":  "input-command",
					"event": raw["event"],
				})
			}

		case "relay-clipboard":
			targetClean, _ := raw["targetClean"].(string)
			rcpt := g.getPeer(CleanDeskID(targetClean))
			if rcpt != nil {
				_ = rcpt.SendJSON(map[string]interface{}{
					"type": "clipboard-sync",
					"text": raw["text"],
				})
			}

		case "relay-file":
			targetClean, _ := raw["targetClean"].(string)
			rcpt := g.getPeer(CleanDeskID(targetClean))
			if rcpt != nil {
				_ = rcpt.SendJSON(map[string]interface{}{
					"type":    "file-transfer",
					"payload": raw["payload"],
				})
			}

		case "session-disconnect":
			targetClean, _ := raw["targetClean"].(string)
			cleanTarget := CleanDeskID(targetClean)
			if g.db != nil {
				g.db.CloseSessionsForPeer(registeredID)
			}
			rcpt := g.getPeer(cleanTarget)
			if rcpt != nil {
				rcpt.SessionWith = ""
				_ = rcpt.SendJSON(map[string]string{
					"type":    "session-closed",
					"message": "Sesi diakhiri oleh rekan koneksi.",
				})
			}
			self := g.getPeer(registeredID)
			if self != nil {
				self.SessionWith = ""
			}
			logger.Info("app.session", "Session terminated by client: id=%s", FormatDeskID(registeredID))

		case "ping":
			timeVal := raw["time"]
			_ = conn.WriteJSON(map[string]interface{}{
				"type": "pong",
				"time": timeVal,
			})
		}
	}
}

func (g *Gateway) StartHeartbeat(interval time.Duration) {
	ticker := time.NewTicker(interval)
	go func() {
		for range ticker.C {
			g.peersMu.RLock()
			peersList := make([]*Peer, 0, len(g.peers))
			for _, p := range g.peers {
				peersList = append(peersList, p)
			}
			g.peersMu.RUnlock()

			now := time.Now().UnixMilli()
			for _, p := range peersList {
				_ = p.SendJSON(map[string]interface{}{
					"type": "heartbeat",
					"time": now,
				})
			}
		}
	}()
}
