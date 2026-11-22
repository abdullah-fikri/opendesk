package dto

// File lane messages (see docs/features/relay-file-lane.md at the repository root). A device asks for a token on
// its main connection, opens a second WebSocket and attaches it with that token as its file lane; the relay then
// forwards file traffic on lanes without video-priority pacing.
const (
	// TypeLaneRequest (client, main connection) asks for a lane token.
	TypeLaneRequest = "file-lane-request"
	// TypeLaneToken (relay, main connection) answers it.
	TypeLaneToken = "file-lane-token"
	// TypeLaneAttach (client) must be the first message of a lane connection.
	TypeLaneAttach = "file-lane-attach"
	// TypeLaneAck (relay, lane connection) confirms the attach.
	TypeLaneAck = "file-lane-ack"
	// TypeLaneLost (relay, main connection) names transfers a dropped lane cut.
	TypeLaneLost = "file-lane-lost"
)

// LaneToken answers file-lane-request: a one-time token with which a second connection of this device may attach
// as its file lane, within a short time and only while this registration lasts.
type LaneToken struct {
	Type  string `json:"type"`
	Token string `json:"token"`
}

// LaneAttach is the first message of a lane connection: the device it belongs to and the token its main
// connection received.
type LaneAttach struct {
	DeskID string `json:"deskId"`
	Token  string `json:"token"`
}

// LaneAck confirms an accepted file-lane-attach on the lane connection.
type LaneAck struct {
	Type string `json:"type"`
}

// LaneLost tells a device that the transfers IDs between it and FromClean that ran over a lane were cut because
// that lane dropped (on either side). IDs are transfer IDs in either direction; the client aborts the matching
// transfers, which then resume.
type LaneLost struct {
	Type      string   `json:"type"`
	FromClean string   `json:"fromClean"`
	IDs       []string `json:"ids"`
}
