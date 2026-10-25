// Package metrics counts relay traffic. Only sizes and message categories are recorded, never payloads.
package metrics

import (
	"runtime"
	"sync/atomic"
	"time"
)

// Ingress message categories.
const (
	KindControl   = "control"
	KindInput     = "input"
	KindFile      = "file"
	KindClipboard = "clipboard"
	KindSignal    = "signal"
	KindAudio     = "audio-bin"
	KindVideo     = "video-bin"
	KindOther     = "other"
)

var ingressKinds = []string{KindControl, KindInput, KindFile, KindClipboard, KindSignal, KindAudio, KindVideo, KindOther}

type counter struct{ messages, bytes atomic.Uint64 }

// Collector aggregates counters. All methods are safe for concurrent use.
type Collector struct {
	ingress         map[string]*counter // fixed key set, written only at construction
	egressMessages  atomic.Uint64
	egressBytes     atomic.Uint64
	droppedFrames   atomic.Uint64
	rejected        atomic.Uint64
	peakPeers       atomic.Int64
	hostListQueries atomic.Uint64
	hostListTotalNs atomic.Uint64
	hostListMaxNs   atomic.Uint64
}

// New returns an empty collector.
func New() *Collector {
	c := &Collector{ingress: make(map[string]*counter, len(ingressKinds))}
	for _, k := range ingressKinds {
		c.ingress[k] = &counter{}
	}
	return c
}

// Ingress counts one received message of the given category; unknown categories count as KindOther.
func (c *Collector) Ingress(kind string, size int) {
	ctr, ok := c.ingress[kind]
	if !ok {
		ctr = c.ingress[KindOther]
	}
	ctr.messages.Add(1)
	ctr.bytes.Add(uint64(size))
}

// Egress counts one message written to a client.
func (c *Collector) Egress(size int) {
	c.egressMessages.Add(1)
	c.egressBytes.Add(uint64(size))
}

// DroppedFrame counts a video frame discarded by a full or restarted queue.
func (c *Collector) DroppedFrame() { c.droppedFrames.Add(1) }

// Rejected counts a message that had no authorized recipient.
func (c *Collector) Rejected() { c.rejected.Add(1) }

// ObservePeers records n connected peers and keeps the peak.
func (c *Collector) ObservePeers(n int) { storeMax(&c.peakPeers, int64(n)) }

// ObserveHostList records the latency of one host-list query.
func (c *Collector) ObserveHostList(d time.Duration) {
	ns := uint64(d.Nanoseconds())
	c.hostListQueries.Add(1)
	c.hostListTotalNs.Add(ns)
	for {
		cur := c.hostListMaxNs.Load()
		if ns <= cur || c.hostListMaxNs.CompareAndSwap(cur, ns) {
			return
		}
	}
}

func storeMax(v *atomic.Int64, n int64) {
	for {
		cur := v.Load()
		if n <= cur || v.CompareAndSwap(cur, n) {
			return
		}
	}
}

// Snapshot returns the counters plus Go runtime figures, ready to encode as JSON.
func (c *Collector) Snapshot(activePeers int) map[string]any {
	ingress := make(map[string]any, len(c.ingress))
	for k, ctr := range c.ingress {
		ingress[k] = map[string]uint64{"messages": ctr.messages.Load(), "bytes": ctr.bytes.Load()}
	}
	var mem runtime.MemStats
	runtime.ReadMemStats(&mem)
	queries := c.hostListQueries.Load()
	avg := uint64(0)
	if queries > 0 {
		avg = c.hostListTotalNs.Load() / queries
	}
	return map[string]any{
		"activePeers":   activePeers,
		"peakPeers":     c.peakPeers.Load(),
		"ingress":       ingress,
		"egress":        map[string]uint64{"messages": c.egressMessages.Load(), "bytes": c.egressBytes.Load()},
		"droppedFrames": c.droppedFrames.Load(),
		"rejected":      c.rejected.Load(),
		"hostList":      map[string]uint64{"queries": queries, "avgNs": avg, "maxNs": c.hostListMaxNs.Load()},
		"goroutines":    runtime.NumGoroutine(),
		"heapAlloc":     mem.HeapAlloc,
		"sys":           mem.Sys,
		"gcCycles":      mem.NumGC,
	}
}
