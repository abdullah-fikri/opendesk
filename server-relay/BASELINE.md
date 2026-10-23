# Relay baseline (partial)

Measured with `go -C server-relay test ./internal/gateway -run '^$' -bench 'BenchmarkRelay(Routing|InputJSON|FileJSON)' -benchtime=300ms -count=1` on Windows/amd64, Intel Core Ultra 5 125U. Single-process microbenchmarks; not end-to-end latency or bandwidth.

| Operation | ns/op | B/op | allocs/op |
| --- | ---: | ---: | ---: |
| Active-session lookup | 31.66 | 0 | 0 |
| JSON input decode | 1816 | 1048 | 26 |
| JSON file-chunk decode | 1964 | 1080 | 28 |

Still needed before relay optimization: repeat under representative concurrency and file/video loads; measure CPU, RSS, network bandwidth, RTT, queue depth, and database host-list latency. No claim of improvement yet. These figures do not justify changing buffer sizes or wire format.
