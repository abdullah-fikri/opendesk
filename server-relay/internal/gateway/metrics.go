package gateway

import (
	"runtime"
	"sync/atomic"
	"time"
)

// Message categories counted on ingress. Payload contents are never recorded.
var ingressKinds = []string{"control", "input", "file", "frame-json", "clipboard", "signal", "audio-bin", "video-bin", "other"}

type counter struct{ messages, bytes atomic.Uint64 }

type relayMetrics struct {
	ingress         map[string]*counter
	egressMessages  atomic.Uint64
	egressBytes     atomic.Uint64
	droppedFrames   atomic.Uint64
	rejected        atomic.Uint64
	peakPeers       atomic.Int64
	hostListQueries atomic.Uint64
	hostListTotalNs atomic.Uint64
	hostListMaxNs   atomic.Uint64
}

var metrics = func() *relayMetrics {
	m := &relayMetrics{ingress: make(map[string]*counter, len(ingressKinds))}
	for _, k := range ingressKinds {
		m.ingress[k] = &counter{}
	}
	return m
}()

func (m *relayMetrics) countIngress(kind string, size int) {
	c, ok := m.ingress[kind]
	if !ok {
		c = m.ingress["other"]
	}
	c.messages.Add(1)
	c.bytes.Add(uint64(size))
}

func (m *relayMetrics) observeHostList(d time.Duration) {
	ns := uint64(d.Nanoseconds())
	m.hostListQueries.Add(1)
	m.hostListTotalNs.Add(ns)
	for {
		cur := m.hostListMaxNs.Load()
		if ns <= cur || m.hostListMaxNs.CompareAndSwap(cur, ns) {
			return
		}
	}
}

func (m *relayMetrics) observePeers(n int) {
	for {
		cur := m.peakPeers.Load()
		if int64(n) <= cur || m.peakPeers.CompareAndSwap(cur, int64(n)) {
			return
		}
	}
}

func ingressKind(msgType string) string {
	switch msgType {
	case "relay-input":
		return "input"
	case "relay-file":
		return "file"
	case "relay-frame":
		return "frame-json"
	case "relay-clipboard":
		return "clipboard"
	case "p2p-signal":
		return "signal"
	case "register", "get-hosts", "connect-request", "approval-response", "session-disconnect", "ping":
		return "control"
	}
	return "other"
}

func (m *relayMetrics) snapshot(activePeers int) map[string]interface{} {
	ingress := make(map[string]interface{}, len(m.ingress))
	for k, c := range m.ingress {
		ingress[k] = map[string]uint64{"messages": c.messages.Load(), "bytes": c.bytes.Load()}
	}
	var mem runtime.MemStats
	runtime.ReadMemStats(&mem)
	queries := m.hostListQueries.Load()
	avg := uint64(0)
	if queries > 0 {
		avg = m.hostListTotalNs.Load() / queries
	}
	return map[string]interface{}{
		"activePeers":   activePeers,
		"peakPeers":     m.peakPeers.Load(),
		"ingress":       ingress,
		"egress":        map[string]uint64{"messages": m.egressMessages.Load(), "bytes": m.egressBytes.Load()},
		"droppedFrames": m.droppedFrames.Load(),
		"rejected":      m.rejected.Load(),
		"hostList":      map[string]uint64{"queries": queries, "avgNs": avg, "maxNs": m.hostListMaxNs.Load()},
		"goroutines":    runtime.NumGoroutine(),
		"heapAlloc":     mem.HeapAlloc,
		"sys":           mem.Sys,
		"gcCycles":      mem.NumGC,
	}
}
