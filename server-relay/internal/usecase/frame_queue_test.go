package usecase

import "testing"

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
