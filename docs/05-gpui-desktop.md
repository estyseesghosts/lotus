# 05 — GPUI desktop application: architecture, UX and rendering

## Framework contract

Use Zed's **GPUI**, not Electron, a web shell or React Native. Build a Rust native app with entities/views/elements, action-driven keyboard shortcuts and async work. The `gpui_platform::application()` entrypoint chooses host platform; GPUI's own README reports Windows Win32/DirectWrite, macOS Metal (`font-kit` for glyphs), Linux Wayland/X11 options. **Pin the GPUI source commit**; the project is pre-1.0 and breaking changes are expected. Source: https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md

## Application entities and data ownership

```text
AppModel
├─ WorkspaceModel             open docks, tabs, saved layouts
├─ SessionList                worker handles + health
│  └─ SessionViewModel        immutable latest snapshots and subscriptions
├─ CaptureStore               immutable capture/artifact metadata
├─ SymbolIndex                per-process ELF/symbol resolvers
├─ DebuggerModel              breakpoints, watches, focused thread, active frame
└─ Preferences                UI / input mapping / privacy
```

The worker never calls into GPUI. A background task reads IPC, validates message ordering/epoch, stores immutable snapshots in a bounded cache and schedules UI model changes via GPUI's thread-safe executor/context mechanisms appropriate to pinned API. Never mutate a GPUI entity from an unmanaged worker thread. Coalesce high-frequency telemetry; don't re-render the entire debugger view for every GPU register change.

## Minimum viable layout

- Central document area: 3DS top/bottom images; layout presets (stacked, side-by-side, single-screen, right-eye, native-resolution view); independently zoom and pan; pixel inspection mode with nearest-neighbor sampling.
- Left dock: sessions + launch presets, process/thread tree, breakpoints/watchpoints.
- Right dock: registers/disassembly, memory browser, PICA state/draw details; searchable, synchronized to selected snapshot.
- Bottom dock: structured event log, tracing status/dropped counts, scripts/test results, audio underruns, worker errors.
- Timeline: separate lanes for CPU execution, SVCs, kernel waits, GPU command lists/draws/transfers, DSP and presentation; show guest clock and host clock independently.
- Header: active instance/build/backend, run/pause/step controls, determinism flag, Old/New 3DS mode, timing model label.

**Avoid** expensive always-live visualizations until a user opens the pane. Closed panes should not trigger costly emulator reads.

## Layout and interaction discipline

- Every panel has an independent stable ID. A GPUI entity maps to an instance and snapshot ID, never a raw C++ pointer.
- Layout persistence is versioned; unknown panel types default to safe fallback rather than losing workspace configuration.
- All numeric addresses appear with process/core/space qualifiers (e.g. CPU virtual `PID:VA`, physical VRAM, GPU register index). Switching current process should not silently reinterpret an old address.
- Suspended/stale snapshots are visibly marked; the UI never shows the latest register contents as though they were captured at the selected old GPU draw.
- Provide keyboard-first navigation and palette commands. Actions should be discoverable and localizable; avoid hardcoding shortcuts in individual panels.
- Display failures/unsupported features explicitly, e.g. `renderer does not support GPU texture capture`, not empty black screenshots.

## Native image integration: do not prematurely force zero-copy

`crates/gpui/src/elements/img.rs` has `ImageSource::Render(Arc<RenderImage>)`, `ImageSource::Image` and `ImageSource::Custom` in the currently inspected source. That supports a first CPU-pixel path with decoded/allocated images. Exact `RenderImage` constructors may change: compile a 2-screen pixel-demo pinned to your chosen GPUI commit before building the worker. Source: https://github.com/zed-industries/zed/blob/main/crates/gpui/src/elements/img.rs

### Phase 1: CPU pixels

1. Worker publishes normalized native-resolution RGB(A) screen images to shared-memory slots.
2. Rust receiver validates descriptor/sequence/format and obtains or copies complete frames.
3. Presenter creates a GPUI renderable image on demand and keeps only the newest displayable frame per active session.
4. UI repaints at host display rate, independent of emulated frame timing; repeated frames should be permitted. Capture path uses raw frames and metadata independently of UI composition.
5. Touch mapping uses *actual bottom-screen transform*: inverse translation/scale/rotation/zoom, clamp to native `320x240`, check pointer-down/move/release semantics, and capture the input device identity.

### Phase 2: GPU acceleration

Azahar's accelerated renderer can produce Vulkan textures, while GPUI's native Windows rendering does not automatically consume them. Proof-of-concept options:

- Vulkan image -> asynchronous GPU readback -> pinned CPU memory -> GPUI image. Simplest reliable progression.
- Vulkan/D3D shared texture interop (Windows): export/import memory and synchronization objects where supported, match adapter/LUID and format, manage ownership/fences and lifetime. Needs native GPUI element/renderer extension, not just wrapping a texture handle in `img()`.
- macOS: investigate Metal bridge/copy path separately; no assumption about Vulkan-to-Metal zero-copy.
- Linux: investigate DMA-BUF/Vulkan interop and GPUI backend support separately.

Every accelerated path must preserve a CPU fallback and report end-to-end display latency including synchronization stalls. Display throughput is not guest hardware timing.

## GPUI performance plan

- Bound update rate of expensive inspectors; separate **display refresh** from **debugger state refresh**.
- Use generation-indexed immutable snapshots and virtualized lists for instruction/event tables; never construct 100k UI rows every frame.
- Asset lifetime: cache only bounded texture previews keyed by `(instance, frame_id, resource_id, epoch)`; evict LRU and invalidate on reset.
- Avoid per-frame image decoding/allocations if a reusable buffer path works; benchmark the chosen GPUI image construction API.
- Render high-cardinality timelines from compact downsampled event bins at the selected zoom; fetch full events only when selected.
- Profile GPUI executor lag, IPC dequeue, image upload, paint time, UI stalls and presentation latency separately from native worker timings.

## Suggested app state transitions

`NoSession -> Starting -> Ready -> Loading -> Running <-> Paused -> Stopping -> Stopped`. Other transitions: `Faulted`, `Disconnected`, `NeedsInput`, `Capturing`. UI action is disabled until legal. `Reset` invalidates old memory/resource generation and pending disassembly/capture requests.

## First compile-only spike

1. Use pinned GPUI commit and official standalone app entrypoint.
2. Show two dynamic generated test patterns, not a ROM.
3. Sustain continuous frame replacement and interactive scaling for 10 minutes; check retained memory; measure CPU upload cost.
4. Simulate 4 sessions with 2 screens each using synthetic changing frames; ensure inactive tabs don't consume similar UI resources as active ones.
5. Draw a virtual 320x240 touch target overlay, click corners, verify inverse transform under zoom and DPI scaling.
6. Suspend/restart frame production without dropping the UI; test worker-initiated format/size changes.

This reduces risk before integrating Azahar's renderer and shows whether GPUI changes require custom Elements.

## Accessibility / quality

Support keyboard navigation, visible focus, screen-reader labels where GPUI exposes them, colorblind-friendly state markers, actual selectable/copyable diagnostic text, accessible high-contrast mode. Debug panes should provide small exportable text artifacts as an alternative to screenshots.
