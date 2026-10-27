# Relay benchmarks

Microbenchmarks of the hot paths, single process. They are not end-to-end latency or bandwidth figures.

```bash
go test -run '^$' -bench . -benchtime=300ms ./internal/handler ./internal/usecase
```

Intel Core Ultra 5 125U, Windows/amd64, Go 1.26:

| Benchmark | What it measures | ns/op | B/op | allocs/op |
| --- | --- | ---: | ---: | ---: |
| `BenchmarkActivePartner` | Authorizing one message (session lookup, both sides) | 21.8 | 0 | 0 |
| `BenchmarkDecodeRelayInput` | Decoding a mouse-move `relay-input` | 1 856 | 608 | 15 |
| `BenchmarkDecodeRelayFileChunk` | Decoding a `relay-file` chunk and its payload header | 2 549 | 888 | 22 |
| `BenchmarkMonitorFanOut` | One 30 KiB monitor-wall thumbnail fanned out to 16 administrators (including their writers) | 3 376 | 810 | 25 |
| `BenchmarkMonitorPage` | One thumbnail from each of the 16 hosts on an administrator's 4x4 page | 2 824 | 262 | 8 |
| `BenchmarkSendInput` | Encoding one relayed mouse move and handing it to the receiver's writer goroutine | 1 590 | 448 | 13 |

`BenchmarkSendInput` was 1 170 ns when the sender wrote the socket itself; the extra ~0.4 µs is the hand-off to
the receiver's writer, which is what keeps a stalled receiver from blocking its sender.

Compared with the 0.x relay, which decoded every message into a generic map: session lookup 31.7 ns before;
input decode 1 816 ns with 26 allocations before (now the same speed with 42 % fewer allocations); file-chunk
decode 1 964 ns before (now about 30 % slower because the payload header is decoded separately, which is
negligible next to sending 32 KiB over the network).

Still missing: measurements under concurrent load (many peers, video and files at once), RSS and CPU per
active session, and host-list query latency on a large registry.
