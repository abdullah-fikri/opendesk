# File grants

Code: `internal/usecase/file_grant.go`; tests in `relay_test.go` and `multi_session_test.go`.

A transfer may take longer than the screen session it started in (the user disconnects while a large file is
still uploading). Grants let it finish without letting anyone else use the channel.

| Action | Authorization |
| --- | --- |
| `start` | Sender and target hold an active session. Creates a grant keyed by `sender:id`, bound to these two exact connections, expiring after 60 s without traffic. At most 8 per sender; duplicate IDs are refused. |
| `chunk`, `end` | A live grant from this sender with this ID; `targetClean`, when given, must match the grant. Each message renews the 60 s window. |
| `ack` | Sent by the grant's receiver to its sender; consumes the grant. |
| `offer`, `pull` | Active session only. They carry no file data; a pull makes the offerer start a normal granted transfer. |

A grant dies when either connection is replaced or removed, or after 60 s idle (the janitor sweeps expired
grants every ping interval). IDs are at most 80 characters. Messages without an authorized receiver are counted
as `rejected` in [metrics](../metrics.md).
