package usecase

import "time"

// frameQueueLen bounds queued video per receiver: deltas need order, but a slow link must not build latency.
const frameQueueLen = 8

// clockBase anchors monoNow; durations since it come from the monotonic clock.
var clockBase = time.Now()

// monoNow is the frame queues' clock: monotonic time since process start.
func monoNow() time.Duration { return time.Since(clockBase) }

// Binary payload markers (see docs/protocol.md at the repository root).
const (
	audioMarker = 0x02 // [0x02, rate_khz, pcm...] or Opus [0x02, 0x4F, seq u16, packet]; the relay only reads byte 0
	jpegMarker0 = 0xFF // FF D8: legacy JPEG or framed video
	jpegMarker1 = 0xD8
)

// Session tag for binary messages to and from a multi-session viewer:
// sessionTag followed by the host's 9-digit clean ID, then the unchanged payload.
const (
	sessionTag    = 0x10
	sessionTagLen = 10
)

// Monitor-wall frames: a host sends monitorTag followed by a plain JPEG; the relay fans it out to every
// watching administrator as monitorTag, the host's 9-digit clean ID, then the JPEG (same layout as sessionTag).
const monitorTag = 0x11

func isAudio(b []byte) bool { return len(b) > 1 && b[0] == audioMarker }

func isVideo(b []byte) bool { return len(b) > 2 && b[0] == jpegMarker0 && b[1] == jpegMarker1 }

// isRestartFrame reports whether a viewer can start decoding at data: legacy JPEGs, framed key frames
// (FF D8 00 4F, kind 1) and H.264 IDR units (kind 3, flag 1). Framed deltas depend on every frame before them.
func isRestartFrame(data []byte) bool {
	if len(data) < 7 || data[2] != 0x00 || data[3] != 0x4F {
		return true
	}
	return data[5] == 1 || (data[5] == 3 && data[6]&1 != 0)
}

// queuedFrame is one queued frame and when it was queued (frameQueue.now).
type queuedFrame struct {
	data []byte
	at   time.Duration
}

// frameQueue is an ordered, bounded video queue for one receiver (or one session of a multi-session viewer).
// Only the sending host's read loop pushes, so drain never races a producer; only the receiver's writer pops.
type frameQueue struct {
	ch      chan queuedFrame
	metrics FrameMetrics
	// wake signals the receiver's writer; nil for a queue no writer serves.
	wake chan struct{}
	// now is the clock frames are stamped and aged with (monoNow; tests replace it).
	now func() time.Duration
	// restartSent and restartDone belong to the writer (pop): restartSent is set when the last frame popped was
	// a restart frame, restartDone is when the writer next came back, i.e. when that frame's write ended.
	restartSent bool
	restartDone time.Duration
}

func newFrameQueue(m FrameMetrics) *frameQueue {
	return &frameQueue{ch: make(chan queuedFrame, frameQueueLen), metrics: m, now: monoNow}
}

// drain empties the queue, counting every discarded frame.
func (q *frameQueue) drain() {
	for {
		select {
		case <-q.ch:
			q.metrics.DroppedFrame()
		default:
			return
		}
	}
}

// push keeps deltas in order and never lets the queue grow past frameQueueLen. A restart frame makes
// everything queued obsolete; a delta that does not fit flushes the queue (the viewer then asks for a key
// frame once) instead of silently breaking the delta chain on every congested frame.
func (q *frameQueue) push(frame []byte) {
	if isRestartFrame(frame) {
		q.drain()
	}
	select {
	case q.ch <- queuedFrame{data: frame, at: q.now()}:
	default:
		q.drain()
		q.metrics.DroppedFrame()
		return
	}
	select {
	case q.wake <- struct{}{}:
	default:
	}
}

// pop takes the oldest queued frame for the writer; false when there is none to send. With maxAge > 0 a delta
// that waited longer than maxAge is dropped, and so is every delta behind it up to the next restart frame: a
// broken chain is flushed exactly like an overflow, and the viewer asks for a key frame once on the gap. Restart
// frames are never dropped for age (they are what ends the gap), and time spent behind a restart frame's write
// does not count, so a key frame that takes long to send on a slow link cannot trigger the next key frame
// request, and the next, in a loop. maxAge 0 keeps the plain bounded queue. Only the writer calls pop.
func (q *frameQueue) pop(maxAge time.Duration) ([]byte, bool) {
	var now time.Duration
	if maxAge > 0 {
		now = q.now()
		if q.restartSent {
			q.restartSent = false
			q.restartDone = now
		}
	}
	broken := false
	for {
		var f queuedFrame
		select {
		case f = <-q.ch:
		default:
			return nil, false
		}
		if maxAge <= 0 {
			return f.data, true
		}
		if isRestartFrame(f.data) {
			q.restartSent = true
			return f.data, true
		}
		if broken || now-max(f.at, q.restartDone) > maxAge {
			q.metrics.DroppedFrame()
			broken = true
			continue
		}
		return f.data, true
	}
}
