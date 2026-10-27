# Metrics

`GET /metrics` returns JSON counters when `OPENDESK_METRICS_TOKEN` (32+ characters) is set and the request
carries `Authorization: Bearer <token>`. Loopback is not trusted on its own: behind a local TLS terminator every
request comes from loopback.

```bash
curl -H "Authorization: Bearer $OPENDESK_METRICS_TOKEN" http://127.0.0.1:8088/metrics
```

| Field | Meaning |
| --- | --- |
| `activePeers`, `peakPeers` | Connected devices now and at most since start |
| `ingress.<kind>.messages/bytes` | Received messages by category: `control`, `input`, `file`, `clipboard`, `signal`, `audio-bin`, `video-bin`, `other` |
| `egress.messages/bytes` | Messages written to clients |
| `droppedFrames` | Video frames discarded by full or restarted queues (congested viewers) |
| `droppedAudio` | Voice packets discarded because a receiver's voice queue was full |
| `slowReceivers` | Connections closed because they stopped taking data with a full control or file queue |
| `rejected` | Binary or file messages without an authorized receiver |
| `hostList.queries/avgNs/maxNs` | Device-registry query count and latency |
| `goroutines`, `heapAlloc`, `sys`, `gcCycles` | Go runtime figures |

Only sizes and categories are counted; payloads are never inspected or stored.
