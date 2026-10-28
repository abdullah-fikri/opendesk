package handler

import (
	"testing"
	"time"
)

func TestSignalBucketAllowsBurstThenPaces(t *testing.T) {
	var b signalBucket
	t0 := time.Now()
	for i := range signalBurst {
		if !b.take(t0) {
			t.Fatalf("signal %d of the burst was dropped", i+1)
		}
	}
	if b.take(t0.Add(50 * time.Millisecond)) {
		t.Fatal("a signal past the burst went through within one interval")
	}
	if !b.take(t0.Add(minSignalInterval + time.Millisecond)) {
		t.Fatal("a signal one interval later was dropped")
	}
}
