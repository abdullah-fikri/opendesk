package usecase

import "testing"

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
