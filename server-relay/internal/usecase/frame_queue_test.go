package usecase

import (
	"testing"
	"time"

	"opendesk/server-relay/internal/metrics"
)

// End-to-end sealed messages (client e2e) keep the bytes the relay routes on in clear: no relay change is
// needed to classify them.
func TestSealedMessagesClassifyLikePlainOnes(t *testing.T) {
	sealedIDR := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 3, 1, 0xE2, 9, 9, 9}
	sealedDelta := []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 2, 0, 0xE2, 9, 9, 9}
	if !isVideo(sealedIDR) || !isRestartFrame(sealedIDR) || isRestartFrame(sealedDelta) {
		t.Fatal("sealed video must keep its kind and IDR flag")
	}
	if !isAudio([]byte{audioMarker, 0xE2, 1, 2, 3}) {
		t.Fatal("sealed voice must stay audio")
	}
	chunk := append([]byte{fileChunkMarker, 2, 'f', '1', 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF}, 9, 9)
	if fileChunkID(chunk) != "f1" {
		t.Fatal("sealed file chunk must keep its transfer ID")
	}
}

// Opus voice keeps the 0x02 marker so every relay version forwards it as audio, never as video.
func TestVoiceClassification(t *testing.T) {
	pcm := []byte{audioMarker, 48, 1, 2}
	opus := []byte{audioMarker, 0x4F, 7, 0, 0x78, 0x01, 0x02}
	for _, b := range [][]byte{pcm, opus} {
		if !isAudio(b) || isVideo(b) {
			t.Fatalf("% x must be audio", b)
		}
	}
	if isAudio([]byte{audioMarker}) || isAudio([]byte{0x03, 0x4F, 0}) {
		t.Fatal("a bare marker or another first byte is not voice")
	}
}

// agedQueue is a frame queue on a fake clock the test advances.
func agedQueue() (*frameQueue, *time.Duration, *metrics.Collector) {
	m := metrics.New()
	q := newFrameQueue(m)
	now := new(time.Duration)
	q.now = func() time.Duration { return *now }
	return q, now, m
}

func testKey() []byte         { return []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 1, 0} }
func testDelta(n byte) []byte { return []byte{0xFF, 0xD8, 0x00, 0x4F, 1, 2, 0, n} }

func TestFramePopWithoutAgeLimitKeepsEveryFrame(t *testing.T) {
	q, now, _ := agedQueue()
	q.push(testDelta(1))
	*now += time.Second
	if f, ok := q.pop(0); !ok || f[7] != 1 {
		t.Fatal("age limit 0 must keep the old behavior")
	}
}

func TestStaleDeltaFlushesTheChainUpToTheNextKeyFrame(t *testing.T) {
	q, now, m := agedQueue()
	q.push(testDelta(1))
	*now += 40 * time.Millisecond
	q.push(testDelta(2))
	*now += 20 * time.Millisecond // delta 1 waited 60 ms, delta 2 only 20 ms
	if _, ok := q.pop(50 * time.Millisecond); ok {
		t.Fatal("a stale delta breaks the chain: the fresh delta behind it is useless too")
	}
	if n := m.Snapshot(0)["droppedFrames"]; len(q.ch) != 0 || n != uint64(2) {
		t.Fatalf("both deltas must be dropped and counted, queued %d, counted %v", len(q.ch), n)
	}

	q.push(testDelta(3))
	*now += 60 * time.Millisecond
	q.push(testKey()) // a key frame flushes the stale delta on push already
	q.push(testDelta(4))
	if f, ok := q.pop(50 * time.Millisecond); !ok || !isRestartFrame(f) {
		t.Fatal("the key frame ends the gap and must be sent")
	}
	if f, ok := q.pop(50 * time.Millisecond); !ok || f[7] != 4 {
		t.Fatal("the delta after the key frame must follow it")
	}
}

func TestFreshFramesPassInOrder(t *testing.T) {
	q, now, _ := agedQueue()
	q.push(testDelta(1))
	q.push(testDelta(2))
	*now += 30 * time.Millisecond
	for want := byte(1); want <= 2; want++ {
		if f, ok := q.pop(50 * time.Millisecond); !ok || f[7] != want {
			t.Fatalf("fresh delta %d must pass in order", want)
		}
	}
}

func TestStaleKeyFrameIsSentAndItsWriteTimeDoesNotCount(t *testing.T) {
	q, now, _ := agedQueue()
	q.push(testKey())
	*now += 200 * time.Millisecond
	if f, ok := q.pop(50 * time.Millisecond); !ok || !isRestartFrame(f) {
		t.Fatal("a key frame is never dropped for age")
	}
	// The key frame's write takes 150 ms on a slow link; deltas queue behind it meanwhile.
	q.push(testDelta(1))
	*now += 150 * time.Millisecond
	q.push(testDelta(2))
	*now += 10 * time.Millisecond // the writer returns here
	if f, ok := q.pop(50 * time.Millisecond); !ok || f[7] != 1 {
		t.Fatal("deltas queued behind a key frame's write must not trigger the next key frame request")
	}
	*now += 70 * time.Millisecond
	if _, ok := q.pop(50 * time.Millisecond); ok {
		t.Fatal("once the key frame is out, the age limit applies again")
	}
}

func TestFramePushAndPopDoNotAllocate(t *testing.T) {
	q, now, _ := agedQueue()
	d := testDelta(1)
	allocs := testing.AllocsPerRun(100, func() {
		q.push(d)
		*now += time.Millisecond
		q.pop(50 * time.Millisecond)
	})
	if allocs != 0 {
		t.Fatalf("the video hot path must not allocate, got %.1f per frame", allocs)
	}
}
