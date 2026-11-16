# T03: inherited private control pipes

Status: **verified compiled** on Windows GNU; process acceptance tests executed.
No Azahar emulation is advertised by this spike. Source baseline:
`11eceda5e0e682a1f44e396bf954a1b9137097ca`; no native files changed.
This records the original T03 spike. The implemented native extension and current
containment behavior are in [21](21-native-control.md).

## Decision

The supervisor creates the worker with inherited stdin/stdout pipe ends. There
is no socket name to guess, public listener, or reconnect operation. Possession
of the inherited handles is the capability. This implements the local-pipe
option without introducing a discoverable named-pipe server. A later attach
feature must add explicit OS access control and secure authentication.

Each message is a four-byte little-endian unsigned byte count followed by UTF-8
JSON. The count must be in 1..=65536 before allocation. Schema:
`specs/control-v1.schema.json`; executable types: `protocol::control`.
The existing fixed C ABI structs are unchanged. The larger protocol draft is
still proposed; the delivered native subset is documented in [21](21-native-control.md).

The connection starts with hello, session identity and epoch 1; status and stop
follow. Replies echo identity, request number and epoch. Unknown fields/methods,
incompatible versions, malformed/truncated/oversized frames and unexpected
reply identities are rejected. A mutable client owns the single outstanding
exchange. Deadlines cover writes as well as reads. On transport failure or
ambiguous timeout the child is killed/reaped; mutations are never retried.
Stderr remains under the supplied private root. Clean stop requires both the
completion reply and successful child exit.

## Reproduction

```powershell
cargo test -p lotus-protocol -p lotus-session -p lotus-cli
cargo build -p lotus-cli -p lotus-session
.\target\debug\ctr-cli.exe mock .\target\debug\lotus-mock-worker.exe
```

Tests cover clean exit and second launch, four simultaneous children, sibling
failure, handshake crash/hang/stale epoch, crash after handshake and framing
limits. The concurrent test exposed duplicate time-derived session UUIDs on
Windows; an atomic counter now guarantees uniqueness within the process.
Identifiers are not authentication secrets.

## Limits and rollback

Linux/macOS are untested. The original T03 spike killed only the direct child;
T09 has since added Windows descendant jobs, storage locks, native capabilities
and emulation commands ([21](21-native-control.md)). The old scaffold
WorkerProcess remains untouched. No guest output/performance claim applies;
only host process/control behavior is exercised. Remove the CLI mock command
and new modules to roll back without changing native behavior.
