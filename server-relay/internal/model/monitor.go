package model

// MonitorState is what an administrator's monitor-wall tile shows for a host (see docs/features/monitor-wall.md
// at the repository root).
type MonitorState string

const (
	MonitorLive        MonitorState = "live"        // the host streams thumbnails
	MonitorOffline     MonitorState = "offline"     // not connected; the tile goes live when it registers
	MonitorUnsupported MonitorState = "unsupported" // the host cannot stream (Android, older desktop clients)
	MonitorDenied      MonitorState = "denied"      // the requester is not an administrator on this relay
)

// MaxMonitorTiles is how many hosts one administrator may watch at once (one 4x4 page of the wall).
const MaxMonitorTiles = 16

// Thumbnail profile the relay asks hosts for in monitor-start. Hosts clamp it to their own limits.
const (
	MonitorMaxWidth = 640
	MonitorFPS      = 2
)
