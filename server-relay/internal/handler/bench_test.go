package handler

import (
	"encoding/base64"
	"encoding/json"
	"testing"

	"opendesk/server-relay/internal/dto"
)

// The hot text path: every mouse move from a viewer is decoded like this.
func BenchmarkDecodeRelayInput(b *testing.B) {
	msg := []byte(`{"type":"relay-input","targetClean":"222222222","event":{"action":"mousemove","normX":0.5,"normY":0.5}}`)
	b.ReportAllocs()
	for b.Loop() {
		var env dto.Envelope
		var req dto.RelayInput
		if json.Unmarshal(msg, &env) != nil || json.Unmarshal(msg, &req) != nil {
			b.Fatal("decode failed")
		}
	}
}

// A full-size JSON chunk (32 KiB as base64), as peers without file-binary send it.
func BenchmarkDecodeRelayFileChunk32K(b *testing.B) {
	payload, _ := json.Marshal(map[string]string{"action": "chunk", "id": "ft_123",
		"chunk": base64.StdEncoding.EncodeToString(make([]byte, 32<<10))})
	msg, _ := json.Marshal(map[string]any{"type": "relay-file", "targetClean": "222222222", "payload": json.RawMessage(payload)})
	b.SetBytes(32 << 10)
	b.ReportAllocs()
	for b.Loop() {
		var env dto.Envelope
		var req dto.RelayFile
		var hdr dto.FilePayloadHeader
		if json.Unmarshal(msg, &env) != nil || json.Unmarshal(msg, &req) != nil || json.Unmarshal(req.Payload, &hdr) != nil {
			b.Fatal("decode failed")
		}
	}
}

// File chunks are the largest text messages in normal use.
func BenchmarkDecodeRelayFileChunk(b *testing.B) {
	msg := []byte(`{"type":"relay-file","targetClean":"222222222","payload":{"action":"chunk","id":"ft_123","chunk":"YWJjZA=="}}`)
	b.ReportAllocs()
	for b.Loop() {
		var env dto.Envelope
		var req dto.RelayFile
		var hdr dto.FilePayloadHeader
		if json.Unmarshal(msg, &env) != nil || json.Unmarshal(msg, &req) != nil || json.Unmarshal(req.Payload, &hdr) != nil {
			b.Fatal("decode failed")
		}
	}
}
