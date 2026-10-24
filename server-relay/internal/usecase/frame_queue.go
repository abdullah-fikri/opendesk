package usecase

// frameQueueLen bounds queued video per receiver: deltas need order, but a slow link must not build latency.
const frameQueueLen = 8

// Binary payload markers (see docs/protocol.md at the repository root).
const (
	audioMarker = 0x02 // [0x02, rate_khz, pcm...]
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

// frameQueue is an ordered, bounded video queue for one receiver (or one session of a multi-session viewer).
// Only the sending host's read loop pushes, so drain never races a producer.
type frameQueue struct {
	ch      chan []byte
	metrics FrameMetrics
}

func newFrameQueue(m FrameMetrics) *frameQueue {
	return &frameQueue{ch: make(chan []byte, frameQueueLen), metrics: m}
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
	case q.ch <- frame:
	default:
		q.drain()
		q.metrics.DroppedFrame()
	}
}
