# Receiving files

Code: `session/FileReceiver.kt`.

Android receives files sent from a PC (it does not send files). Every value from the peer is validated first:

- Transfer IDs: ASCII letters, digits and `_`, 1 to 80 characters; at most 8 transfers at once.
- Names: characters Windows forbids are replaced with `_`; empty, `.`, `..`, control characters and names over
  200 characters are refused. Files up to 2 GB; chunks up to 32 KiB.
- Bytes go to a hidden `.part` file in Downloads (or the app's own download folder when Downloads is not
  writable) and are renamed only when size and SHA-256 match and no file of that name exists.
- Android does not advertise `file-ack`, so the sender reports "not confirmed by the receiver".
