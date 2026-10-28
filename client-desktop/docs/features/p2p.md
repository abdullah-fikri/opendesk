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
   prologue binding it to the viewer and host desk IDs (signal `v: 1`). When both peers list `e2e`, the path
   instead uses the keys of the session's `Noise_XX` handshake with pinned device keys ([e2e.md](e2e.md)) and the
   signal only carries candidates (`v: 2`). NN alone does not stop a relay that rewrites the handshake; XX does.
3. Both sides probe each other's candidates. The first answered probe makes the path active; it is considered
   lost after 3 s without traffic and probing restarts. Candidates are sent again 4 times, every 2 s, because the
   relay may drop a signal.

## UDP relay

Some networks drop every UDP datagram from an address the machine did not send to first, so no probe ever
arrives although both sides know each other's public address. When the relay runs a UDP forwarder (it lists
`udp-relay`) and both clients list `p2p.v1` and `udp-relay`, each side gets a `udp-relay-offer` with the
forwarder's address and its own bind token:

1. The client sends `[0x52][0x01][token]` from its path socket; the relay answers `[0x52][0x03][cookie]` for
   that address, and the client binds with `[0x52][0x01][token][cookie]` until the relay confirms `[0x52][0x02]`,
   then again every 15 s (keeps the NAT mapping and the relay's pair alive). The cookie proves the client
   receives at that address, so a token holder cannot bind a spoofed address and aim its partner's traffic at a
   third party.
2. Once bound, it probes the partner through the forwarder. The forwarder passes datagrams between the two bound
   addresses unread; they are the same encrypted datagrams as on the direct path.
3. The direct route wins whenever it works: while the path runs through the relay, the direct candidates are
   still probed for the usual 60 s and the path moves over on the first answer.

Every NAT lets the answers of a server the machine talks to through, so this works wherever STUN does. The
session overlay shows `Relay (UDP)` for this route. `OPENDESK_UDP_RELAY=0` on a client leaves it out.

## Traffic

Every datagram is at most 1200 bytes, encrypted with an explicit nonce and checked against a 1024-entry replay
window. Video frames are fragmented and reassembled (latest frame wins); the viewer acknowledges each frame,
which opens the host's backpressure gate. Input uses a reliable, ordered channel with retransmission after 80 ms.
Voice is sent as is. Only viewer-to-host input uses the path; host events such as cursor shape and monitors take
the relay.

## Lifetime

A link belongs to one session and closes when it ends or the relay connection drops. `OPENDESK_P2P=0` on a client
or `OPENDESK_P2P_DISABLED=1` on the relay keeps every session on the relay.
