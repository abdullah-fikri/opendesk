package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"time"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/usecase"
)

const (
	// maxMessageBytes: the largest legitimate message is a native-resolution key frame or a file chunk.
	maxMessageBytes = 16 << 20
	// livenessFactor: a peer is dropped after this many missed ping intervals.
	livenessFactor = 4
	// P2P signaling limits: candidates are small and only needed during path setup.
	maxSignalMessageBytes = 4096
	maxCandidateBytes     = 2048
	minSignalInterval     = 100 * time.Millisecond
)

// User-facing messages sent to clients.
const (
	msgInvalidDeskID      = "Invalid desk ID."
	msgInvalidCredentials = "Invalid device credentials."
	msgEnterTargetID      = "Enter a target ID."
	msgSelfConnect        = "You cannot connect to your own device."
	msgTargetViewerOnly   = "Device %s can only view other devices; it cannot be controlled remotely."
	msgTargetUnavailable  = "Device %s was not found or is offline."
	msgRequestRejected    = "The target device rejected the connection request."
	msgPeerOffline        = "The remote device disconnected or went offline."
	msgPeerReconnected    = "The remote device reconnected."
	msgEndedByPeer        = "The session was ended by the remote device."
)

// connection is the per-socket state of one client.
type connection struct {
	h          *Handler
	ws         *websocket.Conn
	conn       *wsConn
	remoteAddr string
	// peer is nil until a successful register.
	peer *usecase.Peer
	// lastSignal rate-limits P2P signaling per session partner, so a multi-session viewer's parallel
	// handshakes do not starve each other.
	lastSignal map[string]time.Time
}

func (h *Handler) serveWebSocket(w http.ResponseWriter, r *http.Request) {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	// Behind a TLS terminator in another network namespace (cloudflared -> Docker) peers arrive from the bridge
	// IP, not loopback; TLSProxy trusts that hop, so the port must be published on 127.0.0.1 only.
	if err != nil || (r.TLS == nil && !net.ParseIP(host).IsLoopback() && !h.opts.TLSProxy) {
		http.Error(w, "WSS required outside loopback", http.StatusForbidden)
		return
	}
	ws, err := h.upgrader.Upgrade(w, r, nil)
	if err != nil {
		h.log.Error("websocket upgrade failed", "addr", r.RemoteAddr, "err", err)
		return
	}
	defer ws.Close()
	ws.SetReadLimit(maxMessageBytes)

	c := &connection{h: h, ws: ws, conn: newWSConn(ws), remoteAddr: r.RemoteAddr, lastSignal: make(map[string]time.Time, 1)}
	liveness := livenessFactor * h.opts.PingInterval
	if liveness > 0 {
		_ = ws.SetReadDeadline(time.Now().Add(liveness))
		ws.SetPongHandler(func(string) error { return ws.SetReadDeadline(time.Now().Add(liveness)) })
	}
	defer c.unregister()

	for {
		kind, msg, err := ws.ReadMessage()
		if err != nil {
			return
		}
		if liveness > 0 {
			_ = ws.SetReadDeadline(time.Now().Add(liveness))
		}
		if kind == websocket.BinaryMessage {
			c.onBinary(msg)
			continue
		}
		if !c.onText(msg) {
			return
		}
	}
}

// onBinary relays voice (0x02 ...), video (FF D8 ...) and file chunks (0x12 ...) without copying.
func (c *connection) onBinary(msg []byte) {
	kind := metrics.KindVideo
	if len(msg) > 0 && msg[0] == 0x02 {
		kind = metrics.KindAudio
	} else if isFileChunk(msg) {
		kind = metrics.KindFile
	}
	c.h.metrics.Ingress(kind, len(msg))
	if c.peer != nil {
		c.h.relay.ForwardBinary(c.peer, msg)
	}
}

// isFileChunk reports a binary file chunk, plain or behind a multi-session viewer's session tag.
func isFileChunk(msg []byte) bool {
	return len(msg) > 0 && msg[0] == 0x12 || len(msg) > 10 && msg[0] == 0x10 && msg[10] == 0x12
}

// onText dispatches one JSON message. It returns false when the connection must be closed.
func (c *connection) onText(msg []byte) bool {
	var env dto.Envelope
	if err := json.Unmarshal(msg, &env); err != nil {
		return true
	}
	c.h.metrics.Ingress(ingressKind(env.Type), len(msg))

	if env.Type == dto.TypeRegister {
		if c.peer != nil {
			return true
		}
		return c.onRegister(msg)
	}
	// Everything else needs a registered connection that has not been replaced by a newer one.
	if !c.h.relay.IsCurrent(c.peer) {
		return true
	}
	switch env.Type {
	case dto.TypeGetHosts:
		c.onGetHosts()
	case dto.TypeConnectRequest:
		c.onConnectRequest(msg)
	case dto.TypeApprovalResponse:
		c.onApprovalResponse(msg)
	case dto.TypeRelayInput:
		c.onRelayInput(msg)
	case dto.TypeRelayClipboard:
		c.onRelayClipboard(msg)
	case dto.TypeRelayFile:
		c.onRelayFile(msg)
	case dto.TypeSessionDisconnect:
		c.onSessionDisconnect(msg)
	case dto.TypeP2PSignal:
		c.onP2PSignal(msg)
	case dto.TypeMonitorSet:
		c.onMonitorSet(msg)
	case dto.TypeSecurityGet, dto.TypeSecuritySet, dto.TypeSecurityAllow, dto.TypeSecurityAllowOnline,
		dto.TypeSecurityRemove, dto.TypeSecurityReset:
		c.onSecurity(env.Type, msg)
	case dto.TypeHostRename:
		c.onHostRename(msg)
	}
	return true
}

func (c *connection) onRegister(msg []byte) bool {
	var req dto.RegisterRequest
	if err := json.Unmarshal(msg, &req); err != nil {
		return true
	}
	info := usecase.PeerInfo{
		CleanID:    model.CleanDeskID(req.DeskID),
		Platform:   valueOr(req.Platform, "device"),
		DeviceName: valueOr(req.DeviceName, "OpenDesk Client"),
		IsAdmin:    req.IsAdmin,
		Caps:       req.Caps,
		RemoteAddr: c.remoteAddr,
	}
	peer, orphans, err := c.h.relay.Register(c.conn, info, req.DeviceToken)
	switch {
	case errors.Is(err, usecase.ErrInvalidDeskID):
		_ = c.ws.WriteJSON(dto.Error{Type: dto.TypeError, Message: msgInvalidDeskID})
		return true
	case errors.Is(err, usecase.ErrUnauthorized):
		// Unprovisioned devices are the usual "it connects but nothing happens" cause; name them for the operator.
		c.h.log.Warn("registration rejected: not on the device allowlist or wrong token",
			"id", model.FormatDeskID(info.CleanID), "addr", c.remoteAddr)
		_ = c.ws.WriteJSON(dto.Error{Type: dto.TypeError, Message: msgInvalidCredentials})
		return false
	case err != nil:
		return true
	}
	// From here on other goroutines write through the peer; direct ws writes would race with them.
	c.peer = peer
	for _, orphan := range orphans {
		_ = orphan.Send(sessionClosed(orphan, peer.CleanID, msgPeerReconnected))
	}
	c.h.log.Info("client registered", "id", peer.DeskID, "name", peer.DeviceName, "platform", peer.Platform,
		"admin", peer.IsAdmin, "addr", c.remoteAddr, "active_peers", c.h.relay.PeerCount())
	_ = peer.Send(dto.RegisteredAck{Type: dto.TypeRegisteredAck, DeskID: peer.DeskID, Caps: c.h.relay.Capabilities()})
	if peer.EnrollToken != "" {
		// First connection of an allowlisted device: from now on it must present this token.
		_ = peer.Send(dto.DeviceCredential{Type: dto.TypeDeviceCredential, Token: peer.EnrollToken})
		c.h.broadcastSecurityState()
	}
	// Administrators already watching this ID see it go live, and it starts streaming thumbnails.
	sendMonitorUpdate(c.h.relay.MonitorAttach(peer))
	c.h.broadcastHostList()
	return true
}

func (c *connection) onGetHosts() {
	if !c.peer.IsAdmin {
		return
	}
	if list, ok := c.h.hostList(); ok {
		_ = c.peer.Send(list)
	}
}

func (c *connection) onConnectRequest(msg []byte) {
	var req dto.ConnectRequest
	if err := json.Unmarshal(msg, &req); err != nil {
		return
	}
	target := model.CleanDeskID(req.TargetID)
	if target == "" {
		_ = c.peer.Send(dto.Error{Type: dto.TypeError, Message: msgEnterTargetID})
		return
	}
	res, err := c.h.relay.Connect(c.peer, target)
	switch {
	case errors.Is(err, usecase.ErrSelfConnect):
		_ = c.peer.Send(dto.Error{Type: dto.TypeError, Message: msgSelfConnect})
		return
	case err != nil:
		message := targetUnavailableMessage(target)
		if errors.Is(err, usecase.ErrTargetViewerOnly) {
			message = fmt.Sprintf(msgTargetViewerOnly, model.FormatDeskID(target))
		}
		c.h.log.Warn("connect request failed", "target", model.FormatDeskID(target), "reason", err)
		failure := dto.Error{Type: dto.TypeError, Message: message}
		if c.peer.Multi {
			// Lets a multi-session viewer fail the right tab (also sent when its session cap is reached).
			failure.TargetClean = target
		}
		_ = c.peer.Send(failure)
		return
	}
	if res.NeedsApproval {
		_ = res.Target.Send(dto.IncomingRequest{
			Type: dto.TypeIncomingRequest, FromID: c.peer.DeskID, FromClean: c.peer.CleanID, DeviceName: c.peer.DeviceName,
		})
	} else {
		_ = res.Target.Send(sessionConnected(c.peer, model.RoleHost))
		_ = c.peer.Send(sessionConnected(res.Target, model.RoleViewer))
	}
	c.h.log.Info("session requested", "viewer", c.peer.DeskID, "host", res.Target.DeskID,
		"platform", res.Target.Platform, "needs_approval", res.NeedsApproval)
}

func (c *connection) onApprovalResponse(msg []byte) {
	var req dto.ApprovalResponse
	if err := json.Unmarshal(msg, &req); err != nil {
		return
	}
	viewerClean := model.CleanDeskID(req.FromClean)
	viewer := c.h.relay.Lookup(viewerClean)
	if viewer == nil {
		return
	}
	if !req.Accept {
		rejected, err := c.h.relay.Reject(c.peer, viewerClean)
		if err != nil {
			return
		}
		failure := dto.Error{Type: dto.TypeError, Message: msgRequestRejected}
		if rejected.Multi {
			failure.TargetClean = c.peer.CleanID
		}
		_ = rejected.Send(failure)
		return
	}
	if err := c.h.relay.Approve(viewer, c.peer); err != nil {
		return
	}
	_ = viewer.Send(sessionConnected(c.peer, model.RoleViewer))
	_ = c.peer.Send(sessionConnected(viewer, model.RoleHost))
	c.h.log.Info("session approved", "viewer", viewer.DeskID, "host", c.peer.DeskID)
}

func (c *connection) onRelayInput(msg []byte) {
	var req dto.RelayInput
	if err := json.Unmarshal(msg, &req); err != nil {
		return
	}
	if rcpt := c.h.relay.ActivePartner(c.peer, model.CleanDeskID(req.TargetClean)); rcpt != nil {
		_ = rcpt.Send(dto.InputCommand{Type: dto.TypeInputCommand, Event: req.Event, FromClean: sourceFor(rcpt, c.peer)})
	}
}

func (c *connection) onRelayClipboard(msg []byte) {
	var req dto.RelayClipboard
	if err := json.Unmarshal(msg, &req); err != nil {
		return
	}
	if rcpt := c.h.relay.ActivePartner(c.peer, model.CleanDeskID(req.TargetClean)); rcpt != nil {
		_ = rcpt.SendClipboard(c.peer.CleanID, dto.ClipboardSync{Type: dto.TypeClipboardSync, Text: req.Text, FromClean: sourceFor(rcpt, c.peer)})
	}
}

// onRelayFile: transfers start only inside an active session; the grant lets them finish after it ends.
func (c *connection) onRelayFile(msg []byte) {
	var req dto.RelayFile
	var hdr dto.FilePayloadHeader
	if json.Unmarshal(msg, &req) != nil || json.Unmarshal(req.Payload, &hdr) != nil {
		c.h.metrics.Rejected()
		return
	}
	rcpt := c.h.relay.RouteFile(c.peer, model.CleanDeskID(req.TargetClean), hdr.Action, hdr.ID)
	if rcpt == nil {
		c.h.metrics.Rejected()
		return
	}
	_ = rcpt.SendFile(c.peer.CleanID, hdr.Action, dto.FileTransfer{Type: dto.TypeFileTransfer, FromClean: c.peer.CleanID, Payload: req.Payload})
}

func (c *connection) onSessionDisconnect(msg []byte) {
	var req dto.SessionDisconnect
	if err := json.Unmarshal(msg, &req); err != nil {
		return
	}
	rcpt := c.h.relay.Disconnect(c.peer, model.CleanDeskID(req.TargetClean))
	if rcpt == nil {
		return
	}
	_ = rcpt.Send(sessionClosed(rcpt, c.peer.CleanID, msgEndedByPeer))
	c.h.log.Info("session ended by client", "id", c.peer.DeskID)
}

// onMonitorSet replaces the hosts an administrator watches. Read-only by construction: a watch is not a
// session, so nothing the administrator sends ever reaches the watched hosts.
func (c *connection) onMonitorSet(msg []byte) {
	var req dto.MonitorSet
	if err := json.Unmarshal(msg, &req); err != nil {
		return
	}
	targets := make([]string, 0, min(len(req.Targets), model.MaxMonitorTiles))
	for _, t := range req.Targets {
		targets = append(targets, model.CleanDeskID(t))
	}
	up, err := c.h.relay.WatchMonitors(c.peer, targets)
	if err != nil {
		c.h.log.Warn("monitor request refused: not an administrator", "id", c.peer.DeskID)
	}
	sendMonitorUpdate(up)
}

func (c *connection) onP2PSignal(msg []byte) {
	if !c.h.relay.P2PEnabled() || len(msg) > maxSignalMessageBytes {
		return
	}
	var req dto.P2PSignal
	if err := json.Unmarshal(msg, &req); err != nil || req.Candidate == "" || len(req.Candidate) > maxCandidateBytes {
		return
	}
	target := model.CleanDeskID(req.TargetClean)
	if time.Since(c.lastSignal[target]) < minSignalInterval {
		return
	}
	rcpt := c.h.relay.ActivePartner(c.peer, target)
	if rcpt == nil {
		return
	}
	if len(c.lastSignal) > 2*model.MaxViewSessions {
		clear(c.lastSignal)
	}
	c.lastSignal[target] = time.Now()
	_ = rcpt.Send(dto.P2PSignalForward{Type: dto.TypeP2PSignal, FromClean: c.peer.CleanID, Candidate: req.Candidate})
}

// unregister runs when the socket closes: it releases the peer's sessions and tells the partners.
func (c *connection) unregister() {
	if c.peer == nil {
		return
	}
	removed, partners := c.h.relay.Unregister(c.peer)
	// Also for a replaced connection: its own monitor-wall tiles end with it.
	sendMonitorUpdate(c.h.relay.MonitorDetach(c.peer))
	if !removed {
		return
	}
	c.h.broadcastHostList()
	for _, partner := range partners {
		_ = partner.Send(sessionClosed(partner, c.peer.CleanID, msgPeerOffline))
	}
	c.h.log.Info("client disconnected", "id", c.peer.DeskID, "active_peers", c.h.relay.PeerCount())
}

// broadcastHostList pushes the registry to every connected administrator, off the caller's goroutine.
func (h *Handler) broadcastHostList() {
	go func() {
		admins := h.relay.Admins()
		if len(admins) == 0 {
			return
		}
		list, ok := h.hostList()
		if !ok {
			return
		}
		for _, admin := range admins {
			_ = admin.Send(list)
		}
	}()
}

func (h *Handler) hostList() (dto.HostList, bool) {
	hosts, err := h.relay.HostList(context.Background())
	if err != nil {
		h.log.Error("host list query failed", "err", err)
		return dto.HostList{}, false
	}
	return hostListDTO(hosts), true
}

func valueOr(v, fallback string) string {
	if v == "" {
		return fallback
	}
	return v
}

func ingressKind(msgType string) string {
	switch msgType {
	case dto.TypeRelayInput:
		return metrics.KindInput
	case dto.TypeRelayFile:
		return metrics.KindFile
	case dto.TypeRelayClipboard:
		return metrics.KindClipboard
	case dto.TypeP2PSignal:
		return metrics.KindSignal
	case dto.TypeRegister, dto.TypeGetHosts, dto.TypeConnectRequest, dto.TypeApprovalResponse, dto.TypeSessionDisconnect,
		dto.TypeMonitorSet, dto.TypeSecurityGet, dto.TypeSecuritySet, dto.TypeSecurityAllow, dto.TypeSecurityAllowOnline,
		dto.TypeSecurityRemove, dto.TypeSecurityReset, dto.TypeHostRename:
		return metrics.KindControl
	}
	return metrics.KindOther
}
