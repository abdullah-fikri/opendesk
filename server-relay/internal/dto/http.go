package dto

// Status is the body of GET / on the relay.
type Status struct {
	Service     string `json:"service"`
	Version     string `json:"version"`
	Status      string `json:"status"`
	Storage     string `json:"storage"`
	ActivePeers int    `json:"activePeers"`
	Uptime      int    `json:"uptime"`
}

// Health is the body of GET /healthz.
type Health struct {
	Status string `json:"status"`
}
