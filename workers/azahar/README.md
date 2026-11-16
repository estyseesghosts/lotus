# Native Azahar worker

This directory holds the C++ worker: one process per emulator instance,
running the pinned Azahar core behind a minimal frontend.

## Status

**Proposed.** The worker currently builds as a protocol stub: it prints its
handshake and exits. The frontend that replaces the Azahar Qt frontend is
described in [docs/04-azahar-native-worker.md](../../docs/04-azahar-native-worker.md)
and [docs/16-azahar-patch-blueprint.md](../../docs/16-azahar-patch-blueprint.md).

## What the worker owns

- The Azahar core: CPU, kernel, GPU, DSP, loaders, services, saves.
- Its own storage: NAND, SD, saves, logs and captures.
- Its own debugger session and command queue.
- The frame ring in shared memory and the event stream.

## What the worker must not know about

GPUI, MCP, agents, application layout. The worker is a service, not a UI.

## ABI

`include/lotus_protocol.h` mirrors `crates/protocol/src/ffi.rs`. Both sides
static assert the same struct sizes so the ABI cannot drift silently.

## Building

```powershell
# Protocol stub, no Azahar needed.
cmake -S workers/azahar -B build/worker-stub
cmake --build build/worker-stub

# Against the pinned Azahar tree.
cmake -S workers/azahar -B build/azahar-integration `
      -DLOTUS_AZAHAR_ROOT="$PWD/vendor/azahar" `
      -DCMAKE_BUILD_TYPE=Release
cmake --build build/azahar-integration
```

Use one build directory per branch. Never point the integration and the
experimental build at the same CMake cache.
