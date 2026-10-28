# End-to-end encryption

Code: `src/infra/identity.rs` (device key), `src/infra/key_pins.rs` (pinned host keys), `src/infra/e2e.rs`
(handshake and sealing), wired in `src/service/relay/dispatcher.rs` and `connection.rs`.

Without it the relay sees every session in clear (TLS ends at the relay) and could answer the P2P `Noise_NN`
handshake itself. With it, a relay that forwards a session sees only the routing fields it needs.

## Device key

Each installation creates one X25519 keypair on first use and keeps it in `identity.json` in the app directory.
Windows seals the private key with DPAPI, machine scope (the service's SYSTEM agent and the app the user starts
read the same file); the file sits in the user's profile, whose ACL keeps other users out. Linux writes the file
with mode 0600. `opendesk status` prints the key's fingerprint (5 groups of 4 hex digits of its SHA-256) so it
can be compared out of band. An unreadable key is replaced by a new one, which viewers then see as a changed key.

## Handshake and pinning

Used when both peers list `e2e` and the relay lists `p2p-signal`. Otherwise nothing changes: P2P keeps
`Noise_NN`, relay traffic stays plain.

1. Viewer and host run `Noise_XX_25519_ChaChaPoly_BLAKE2s` with their device keys over `p2p-signal`
   (`{"v":2,"e2e":base64}`), prologue `opendesk-e2e-v1|<viewer>|<host>`.
2. After the host's message the viewer checks the host key against `known_hosts.json` (host clean ID to key).
   First contact pins it. A different key ends the handshake and closes the session with a warning naming
   both fingerprints. `opendesk trust <id>` forgets the pin, so the next connection pins the new key.
3. Hosts accept every viewer key (unattended hosts have no access control) and only log it.
4. The host seals what it sends once it has read the last handshake message and answers `{"v":2,"e2e-ready":true}`;
   the viewer seals from that answer on, so nothing sealed reaches a host that cannot open it yet.

A viewer whose host had a pinned key but now does not offer `e2e` is warned in the status line.
`OPENDESK_E2E=0` turns it off on a client (no capability, no handshake).

## What is sealed

The handshake's transport keys protect, with one nonce domain (top byte of the 64-bit nonce) each:

| Domain | Traffic |
| --- | --- |
| 0 | P2P datagrams (instead of the NN keys) |
| 1 | relay video (framed key, delta, H.264) |
| 2 | relay voice (PCM and Opus) |
| 3 | relay binary file chunks |
| 4 | relay input events (`relay-input`, both directions) |

A sealed binary message keeps the bytes the relay routes on and marks itself:

```text
video [FF D8 00 4F version kind flags 0xE2][nonce u64][segments]   reserved byte 7 = 0xE2
voice [0x02 0xE2][nonce u64][segments]                              0xE2 is never a rate or 0x4F
file  [0x12 idlen id FF FF FF FF FF FF FF FF][nonce u64][segments]  real offset is inside
```

The segments seal `[length u32][original message]` in pieces of at most one Noise message (65519 bytes), with
consecutive nonces. The original header is inside too, so a relay that changes a clear byte (for example a delta
relabeled as key frame) is detected. Input events become `{"action":"e2e","d":base64(nonce, segments)}`.

Each stream arrives in order through its relay queue, so the receiver accepts only increasing nonces per stream.
Once a stream delivered a sealed message, plain messages on it are dropped (an injected frame or keystroke).

## What the relay still sees

- Who talks to whom, when, message sizes and timing; video kind, IDR flag and the moment of key frames.
- File transfer metadata: `relay-file` JSON (`start` name and size, `end` hash, offers) and chunk IDs.
- Clipboard text (`relay-clipboard`), control messages and P2P candidates.
- Legacy plain JPEG video and monitor-wall thumbnails (`0x11`, not a session).

## Rollout

Old clients never list `e2e`, so they keep the old behavior in both directions. The relay needs no change: it
reads only bytes that stay in clear (`server-relay/internal/usecase/frame_queue_test.go` pins this).
