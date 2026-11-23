package handler

import (
	"encoding/json"

	"github.com/gorilla/websocket"

	"opendesk/server-relay/internal/dto"
	"opendesk/server-relay/internal/metrics"
	"opendesk/server-relay/internal/model"
	"opendesk/server-relay/internal/usecase"
)

// File lane transport (see usecase/lane.go and docs/features/relay-file-lane.md at the repository root). A lane is
// a connection to the same endpoint as the main one, so it passes the same TLS check and any proxy in front of
// the relay needs no change; its first message is file-lane-attach instead of register.

// msgLaneRefused is sent before a lane connection with a wrong, spent or expired token is closed.
const msgLaneRefused = "File lane refused."

// onLaneRequest issues a lane token to the main connection.
func (c *connection) onLaneRequest() {
	token, err := c.h.relay.IssueLaneToken(c.peer)
	if err != nil {
		return
	}
	_ = c.peer.Send(dto.LaneToken{Type: dto.TypeLaneToken, Token: token})
}

// onLaneAttach turns this connection into the file lane of a registered device. It returns false (close the
// connection) when the device or token does not check out.
func (c *connection) onLaneAttach(msg []byte) bool {
	var req dto.LaneAttach
	if err := json.Unmarshal(msg, &req); err != nil {
		return false
	}
	clean := model.CleanDeskID(req.DeskID)
	lane, lost, err := c.h.relay.AttachLane(c.conn, clean, req.Token)
	if err != nil {
		c.h.log.Warn("file lane refused", "id", model.FormatDeskID(clean), "addr", c.remoteAddr)
		_ = c.ws.WriteJSON(dto.Error{Type: dto.TypeError, Message: msgLaneRefused})
		return false
	}
	// From here on the lane's writer owns the socket.
	c.lane = lane
	c.h.sendLaneLost(lost)
	_ = lane.Send(dto.LaneAck{Type: dto.TypeLaneAck})
	c.h.log.Info("file lane attached", "id", lane.Owner.DeskID, "addr", c.remoteAddr)
	return true
}

// onLaneMessage handles one message on a lane: JSON start, chunk and end of a transfer, or a binary chunk.
func (c *connection) onLaneMessage(kind int, msg []byte) {
	c.h.metrics.Ingress(metrics.KindFile, len(msg))
	if kind == websocket.BinaryMessage {
		c.h.relay.ForwardLaneBinary(c.lane, msg)
		return
	}
	var req dto.RelayFile
	var hdr dto.FilePayloadHeader
	if json.Unmarshal(msg, &req) != nil || json.Unmarshal(req.Payload, &hdr) != nil {
		c.h.metrics.Rejected()
		return
	}
	rcpt, out := c.h.relay.RouteLaneFile(c.lane, model.CleanDeskID(req.TargetClean), hdr.Action, hdr.ID)
	if rcpt == nil {
		c.h.metrics.Rejected()
		return
	}
	from := c.lane.Owner.CleanID
	v := dto.FileTransfer{Type: dto.TypeFileTransfer, FromClean: from, Payload: req.Payload}
	if out != nil {
		_ = out.SendFile(from, v)
		return
	}
	_ = rcpt.SendFile(from, hdr.Action, v)
}

// detachLane runs when a lane connection ends.
func (c *connection) detachLane() {
	c.h.sendLaneLost(c.h.relay.DetachLane(c.lane))
	c.h.log.Info("file lane closed", "id", c.lane.Owner.DeskID)
}

// sendLaneLost tells each partner which transfers a dropped lane cut.
func (h *Handler) sendLaneLost(lost []usecase.LaneLost) {
	for _, l := range lost {
		_ = l.To.Send(dto.LaneLost{Type: dto.TypeLaneLost, FromClean: l.From, IDs: l.IDs})
	}
}
