package gateway

import (
	"encoding/json"
	"fmt"
	"net"
	"regexp"
	"sync"
	"time"

	"github.com/gorilla/websocket"
)

var nonDigits = regexp.MustCompile(`\D`)

func CleanDeskID(id string) string {
	return nonDigits.ReplaceAllString(id, "")
}

func FormatDeskID(raw string) string {
	digits := CleanDeskID(raw)
	if len(digits) >= 9 {
		return fmt.Sprintf("%s %s %s", digits[0:3], digits[3:6], digits[6:9])
	}
	return digits
}

type framePacket struct {
	isBinary bool
	data     []byte
	jsonMsg  map[string]interface{}
}

type Peer struct {
	CleanID       string
	DeskID        string
	Platform      string
	DeviceName    string
	IsAdmin       bool
	SessionWith   string
	SessionActive bool
	Caps          []string
	WS            *websocket.Conn
	writeMu       sync.Mutex
	frameCh       chan framePacket
	closeCh       chan struct{}
	closeOnce     sync.Once

	// Multi is set at registration when the client advertises capMultiSession; it never changes afterwards.
	Multi bool
	// views holds a multi-session viewer's sessions keyed by host clean ID (guarded by Gateway.peersMu).
	// Legacy peers and the host role keep using SessionWith/SessionActive.
	views map[string]*viewSession
}

func NewPeer(cleanID, platform, deviceName string, isAdmin bool, ws *websocket.Conn) *Peer {
	if tcpConn, ok := ws.UnderlyingConn().(*net.TCPConn); ok {
		_ = tcpConn.SetNoDelay(true)
	}
	p := &Peer{
		CleanID:    cleanID,
		DeskID:     FormatDeskID(cleanID),
		Platform:   platform,
		DeviceName: deviceName,
		IsAdmin:    isAdmin,
		WS:         ws,
		frameCh:    make(chan framePacket, frameQueueLen),
		closeCh:    make(chan struct{}),
	}
	go p.frameWriterLoop()
	return p
}

func (p *Peer) Close() {
	p.closeOnce.Do(func() {
		close(p.closeCh)
	})
}

func (p *Peer) frameWriterLoop() {
	for {
		select {
		case <-p.closeCh:
			return
		case pkt := <-p.frameCh:
			if pkt.isBinary {
				_ = p.SendRawBinary(pkt.data)
			} else if pkt.jsonMsg != nil {
				_ = p.SendJSON(pkt.jsonMsg)
			}
		}
	}
}

// frameQueueLen bounds queued video per viewer: deltas need order, but a slow link must not build latency.
const frameQueueLen = 8

// isRestartFrame reports whether a viewer can start decoding at data: legacy JPEGs, framed key frames
// (FF D8 00 4F, kind 1) and H.264 IDR units (kind 3, flag 1). Framed deltas depend on every frame before them.
func isRestartFrame(data []byte) bool {
	if len(data) < 7 || data[2] != 0x00 || data[3] != 0x4F {
		return true
	}
	return data[5] == 1 || (data[5] == 3 && data[6]&1 != 0)
}

// drainFrames empties a queue; only the sending host's read loop enqueues, so this cannot race a producer.
func drainFrames(ch chan framePacket) {
	for {
		select {
		case <-ch:
			metrics.droppedFrames.Add(1)
		default:
			return
		}
	}
}

// pushFrame keeps deltas in order and never lets the queue grow past frameQueueLen. A restart frame makes
// everything queued obsolete; a delta that does not fit flushes the queue (the viewer then asks for a key
// frame once) instead of silently breaking the delta chain on every congested frame.
func pushFrame(ch chan framePacket, pkt framePacket, restart bool) {
	if restart {
		drainFrames(ch)
	}
	select {
	case ch <- pkt:
	default:
		drainFrames(ch)
		metrics.droppedFrames.Add(1)
	}
}

func (p *Peer) enqueueFrame(pkt framePacket, restart bool) { pushFrame(p.frameCh, pkt, restart) }

// EnqueueBinaryFrame queues a video frame for the viewer (see enqueueFrame).
func (p *Peer) EnqueueBinaryFrame(data []byte) {
	p.enqueueFrame(framePacket{isBinary: true, data: data}, isRestartFrame(data))
}

// EnqueueJSONFrame keeps latest-frame-wins for legacy JSON base64 frames (each is a full image).
func (p *Peer) EnqueueJSONFrame(msg map[string]interface{}) {
	p.enqueueFrame(framePacket{isBinary: false, jsonMsg: msg}, true)
}

// viewSession is one session of a multi-session viewer. Each has its own ordered frame queue so a key frame or
// congestion from one host never flushes another host's delta chain.
type viewSession struct {
	active  bool // guarded by Gateway.peersMu
	frameCh chan framePacket
	done    chan struct{}
}

// startView creates a session queue whose writer tags every binary message with the host's clean ID.
func (p *Peer) startView(hostClean string, active bool) *viewSession {
	v := &viewSession{active: active, frameCh: make(chan framePacket, frameQueueLen), done: make(chan struct{})}
	go p.viewWriterLoop(v, hostClean)
	return v
}

func (p *Peer) viewWriterLoop(v *viewSession, hostClean string) {
	for {
		select {
		case <-p.closeCh:
			return
		case <-v.done:
			return
		case pkt := <-v.frameCh:
			if pkt.isBinary {
				_ = p.SendTaggedBinary(hostClean, pkt.data)
			} else if pkt.jsonMsg != nil {
				_ = p.SendJSON(pkt.jsonMsg)
			}
		}
	}
}

func (v *viewSession) enqueueBinary(data []byte) {
	pushFrame(v.frameCh, framePacket{isBinary: true, data: data}, isRestartFrame(data))
}

func (v *viewSession) enqueueJSON(msg map[string]interface{}) {
	pushFrame(v.frameCh, framePacket{jsonMsg: msg}, true)
}

// Binary messages to and from a multi-session viewer carry a session tag: binTag followed by the host's
// 9-digit clean ID, then the unchanged legacy payload (0x02 audio or FF D8 video).
const (
	binTag    = 0x10
	binTagLen = 10
)

// writeTimeout bounds how long a stalled receiver can block the sender's read loop.
const writeTimeout = 10 * time.Second

func (p *Peer) write(messageType int, data []byte) error {
	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	_ = p.WS.SetWriteDeadline(time.Now().Add(writeTimeout))
	err := p.WS.WriteMessage(messageType, data)
	if err != nil {
		// A failed write leaves the connection unusable; closing it ends the peer's read loop and cleanup runs.
		_ = p.WS.Close()
		return err
	}
	metrics.egressMessages.Add(1)
	metrics.egressBytes.Add(uint64(len(data)))
	return nil
}

// SendTaggedBinary writes binTag+hostClean+data as one message without copying data into a new buffer.
func (p *Peer) SendTaggedBinary(hostClean string, data []byte) error {
	var hdr [binTagLen]byte
	hdr[0] = binTag
	copy(hdr[1:], hostClean)
	p.writeMu.Lock()
	defer p.writeMu.Unlock()
	_ = p.WS.SetWriteDeadline(time.Now().Add(writeTimeout))
	w, err := p.WS.NextWriter(websocket.BinaryMessage)
	if err == nil {
		if _, err = w.Write(hdr[:]); err == nil {
			_, err = w.Write(data)
		}
		if cerr := w.Close(); err == nil {
			err = cerr
		}
	}
	if err != nil {
		_ = p.WS.Close()
		return err
	}
	metrics.egressMessages.Add(1)
	metrics.egressBytes.Add(uint64(binTagLen + len(data)))
	return nil
}

func (p *Peer) SendJSON(v interface{}) error {
	data, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return p.write(websocket.TextMessage, data)
}

func (p *Peer) SendRawText(data []byte) error {
	return p.write(websocket.TextMessage, data)
}

func (p *Peer) SendRawBinary(data []byte) error {
	return p.write(websocket.BinaryMessage, data)
}

// SendPing uses a control frame; gorilla allows WriteControl concurrently with other writes.
func (p *Peer) SendPing() error {
	return p.WS.WriteControl(websocket.PingMessage, nil, time.Now().Add(writeTimeout))
}
