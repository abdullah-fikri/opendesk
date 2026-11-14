// Package memlimit gives the Go garbage collector a soft memory limit derived from the container's cgroup
// memory limit, so a relay in a small container collects garbage harder before the kernel OOM-kills it.
package memlimit

import (
	"bytes"
	"log/slog"
	"math"
	"os"
	"runtime/debug"
	"strconv"
)

// Cgroup files holding the container's memory limit: v2 (unified) first, then v1.
const (
	cgroupV2Limit = "/sys/fs/cgroup/memory.max"
	cgroupV1Limit = "/sys/fs/cgroup/memory/memory.limit_in_bytes"
)

// unlimitedV1 is the smallest cgroup v1 value that means "no limit": v1 reports the page-aligned maximum
// int64 (9223372036854771712) instead of a word like v2's "max".
const unlimitedV1 = int64(1) << 62

// ratioPercent of the container limit becomes the GC limit, leaving room for goroutine stacks, socket buffers
// and memory the runtime has not returned yet.
const ratioPercent = 80

// Apply sets the GC memory limit to 80 % of the cgroup memory limit and logs the decision once. It changes
// nothing when GOMEMLIMIT (or anything else) already set a limit, when no cgroup limit is found (no container,
// "max", unreadable files, non-Linux), or when the limit is too small to be real.
func Apply(log *slog.Logger) {
	if current := debug.SetMemoryLimit(-1); current != math.MaxInt64 {
		log.Info("GC memory limit kept", "limit_bytes", current, "source", "GOMEMLIMIT")
		return
	}
	limit, path, ok := cgroupLimit(os.ReadFile)
	if !ok {
		log.Info("GC memory limit not set", "reason", "no cgroup memory limit")
		return
	}
	target := gcLimit(limit)
	if target <= 0 {
		log.Info("GC memory limit not set", "reason", "cgroup memory limit too small", "cgroup_bytes", limit)
		return
	}
	debug.SetMemoryLimit(target)
	log.Info("GC memory limit set", "limit_bytes", target, "cgroup_bytes", limit, "source", path)
}

// cgroupLimit returns the first finite memory limit found in the v2 or v1 cgroup file, read with read, and the
// file it came from.
func cgroupLimit(read func(string) ([]byte, error)) (int64, string, bool) {
	for _, path := range []string{cgroupV2Limit, cgroupV1Limit} {
		data, err := read(path)
		if err != nil {
			continue
		}
		if limit, ok := parseLimit(data); ok {
			return limit, path, true
		}
	}
	return 0, "", false
}

// parseLimit parses the content of a cgroup memory limit file. "max" (v2), the v1 "unlimited" value, zero,
// negative and malformed values report no limit.
func parseLimit(data []byte) (int64, bool) {
	s := string(bytes.TrimSpace(data))
	if s == "" || s == "max" {
		return 0, false
	}
	n, err := strconv.ParseInt(s, 10, 64)
	if err != nil || n <= 0 || n >= unlimitedV1 {
		return 0, false
	}
	return n, true
}

// gcLimit is the GC memory limit for a container limit of limit bytes; zero for a limit below 16 MiB, which
// leaves no useful headroom.
func gcLimit(limit int64) int64 {
	if limit < 16<<20 {
		return 0
	}
	return limit / 100 * ratioPercent
}
