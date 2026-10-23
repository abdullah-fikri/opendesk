# Direct P2P path

Code: `src/infra/p2p.rs`; started by `src/service/relay/dispatcher.rs` when both peers advertise `p2p.v1` and the
relay advertises `p2p-signal`.

The relay stays the control plane: it authenticates devices, approves the session and carries the handshake.
The direct path only moves video, voice and input, and every session falls back to the relay automatically.

## Setup

1. Each side opens a UDP socket and gathers candidates: LAN addresses, plus its public address from STUN
   (`OPENDESK_STUN`, default `stun.l.google.com:19302`; `off` disables it). At most 8; loopback, multicast and
   ports below 1024 are rejected.
2. A Noise `NN_25519_ChaChaPoly_BLAKE2s` handshake travels in `p2p-signal` messages through the relay, with a
   prologue binding it to the viewer and host desk IDs.
3. Both sides probe each other's candidates. The first answered probe makes the path active; it is considered
   lost after 3 s without traffic and probing restarts.

## Traffic

Every datagram is at most 1200 bytes, encrypted with an explicit nonce and checked against a 1024-entry replay
window. Video frames are fragmented and reassembled (latest frame wins); the viewer acknowledges each frame,
which opens the host's backpressure gate. Input uses a reliable, ordered channel with retransmission after 80 ms.
Voice is sent as is. Only viewer-to-host input uses the path; host events such as cursor shape and monitors take
the relay.

## Lifetime

A link belongs to one session and closes when it ends or the relay connection drops. `OPENDESK_P2P=0` on a client
or `OPENDESK_P2P_DISABLED=1` on the relay keeps every session on the relay.
