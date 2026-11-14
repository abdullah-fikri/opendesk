package memlimit

import (
	"errors"
	"io/fs"
	"testing"
)

func TestParseLimit(t *testing.T) {
	cases := []struct {
		in   string
		want int64
		ok   bool
	}{
		{"524288000\n", 524288000, true},
		{"max\n", 0, false},
		{"9223372036854771712\n", 0, false}, // cgroup v1 "unlimited"
		{"", 0, false},
		{"0", 0, false},
		{"-1", 0, false},
		{"garbage", 0, false},
	}
	for _, c := range cases {
		got, ok := parseLimit([]byte(c.in))
		if got != c.want || ok != c.ok {
			t.Errorf("parseLimit(%q) = %d, %v; want %d, %v", c.in, got, ok, c.want, c.ok)
		}
	}
}

func TestGCLimitIsEightyPercent(t *testing.T) {
	if got := gcLimit(500 << 20); got != 419430400 {
		t.Fatalf("500 MiB container: got %d", got)
	}
	if got := gcLimit(8 << 20); got != 0 {
		t.Fatalf("a tiny limit must be ignored, got %d", got)
	}
}

func TestCgroupLimitPrefersV2AndFallsBackToV1(t *testing.T) {
	files := map[string]string{cgroupV2Limit: "max\n", cgroupV1Limit: "524288000\n"}
	read := func(p string) ([]byte, error) {
		if s, ok := files[p]; ok {
			return []byte(s), nil
		}
		return nil, fs.ErrNotExist
	}
	if limit, path, ok := cgroupLimit(read); !ok || limit != 524288000 || path != cgroupV1Limit {
		t.Fatalf("v2 'max' must fall back to v1, got %d %q %v", limit, path, ok)
	}
	files[cgroupV2Limit] = "1048576000\n"
	if limit, path, ok := cgroupLimit(read); !ok || limit != 1048576000 || path != cgroupV2Limit {
		t.Fatalf("v2 limit must win, got %d %q %v", limit, path, ok)
	}
	none := func(string) ([]byte, error) { return nil, errors.New("unreadable") }
	if _, _, ok := cgroupLimit(none); ok {
		t.Fatal("unreadable files must mean no limit")
	}
}
