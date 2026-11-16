# **A new 3DS emulator: architecture and bootstrap plan**

I think the right approach is to **build a new emulator platform in Rust, initially powered by a modified Azahar core, while developing independent hardware models alongside it.**

This gets you something useful relatively early without spending months recreating functionality Citra already solved.

The long-term goal would be an independent, hardware-accurate emulator. But the first milestone should be a genuinely useful replacement for your current Azahar debugging workflow.

## **1. The architecture I'd choose**

**Rust / GPUI desktop application**

Game display · debugger · profiler · instance manager · trace viewer

**Rust control service**

Sessions · commands · input · snapshots

**MCP server**

Agent tools · test automation · artifact access

Versioned IPC + shared-memory frame buffers

**Worker A**

Independent OS process

Emulation backend

Azahar initially

CPU · Kernel · GPU · DSP

Private RAM · NAND · SD · saves

**Worker B**

Independent OS process

Emulation backend

Azahar initially

CPU · Kernel · GPU · DSP

Private RAM · NAND · SD · saves

**Hardware research and validation**

Coffeepot · 3dbrew · hardware tests · accuracy fixtures · reference captures

The important design decision is that **GPUI, the debugger, and MCP aren't part of the emulator core**. They are clients of a separate emulation service.

That makes it possible to run the emulator without a GUI, control multiple instances independently, and eventually replace individual hardware implementations without rewriting your tools.

It also means we can retain the existing C++ emulation code without forcing the entire new application to be written in C++.

## **2. What the existing projects give us**

I checked the current Azahar source, the archived Citra wiki, the GPUI repository, and Coffeepot's CTR documentation.

| Source                                                                                            | What I'd take from it                                                                          |
| ------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| Azahar                                                                                            | Working CPU, kernel, GPU, DSP, loaders, services, saves, Vulkan backend                        |
| Citra                                                                                             | Historical design decisions, debugging approaches, architecture documentation                  |
| Mikage                                                                                            | Independent CPU/GPU implementation ideas, system-software emulation, existing debugger designs |
| [3dbrew](https://www.3dbrew.org/wiki/Main_Page?utm_source=chatgpt.com)                            | Registers, memory maps, hardware behavior, firmware, kernel documentation                      |
| [Coffeepot CTR](https://github.com/estyseesghosts/coffeepot/tree/main/ctr?utm_source=chatgpt.com) | Organized hardware research, performance experiments, test methodology                         |
| [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui?utm_source=chatgpt.com)        | Native Rust desktop UI, workspaces, panels, graphics presentation                              |

The old [Citra wiki](https://citra.azahar-emu.org/wiki/home/?utm_source=chatgpt.com) remains available through Azahar's archive, while the active implementation is in Azahar.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://citra.azahar-emu.org\&sz=32)

Citra (Website Archive)+1



### **Why Azahar is the best bootstrap base**

Its current CMake configuration exposes some very useful pieces:

- A Libretro frontend, which already separates emulation from a conventional desktop UI.
- A software graphics renderer alongside OpenGL and Vulkan.
- An RPC scripting server.
- An ARM GDB stub.
- Frame timing, debugging, and profiling facilities.
- Configurable deterministic asynchronous operations.

These are present in Azahar's current build configuration, rather than features we'd need to invent.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub



More importantly, its existing `Frontend::EmuWindow` abstraction already supports a headless window-system type. The Vulkan backend explicitly recognizes headless operation.

That gives us a real integration point for a custom worker. We don't need to begin by replacing Qt with GPUI throughout Azahar.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub



### **What I would not reuse unchanged**

The current Azahar source still declares a static `Core::System::s_instance`, and some core facilities resolve their state through process-wide globals.

That is a problem for multiple emulator instances inside one process.

Trying to remove all those assumptions immediately would introduce substantial code churn.

Instead, run **one Azahar-derived core per worker process**. Each worker gets its own global state, configuration, storage, debugger, and renderer.

That gives us parallelism and crash isolation now, while preserving the possibility of proper in-process multi-instance support later.

## **3. Bootstrapping the emulation core**

I would keep two distinct layers of implementation:

**Rust owns the emulator's architecture and tooling. C++ temporarily owns the actual emulation.**

This is a migration strategy, not an attempt to rewrite Azahar in Rust through automated translation.

### **The first worker**

Create a small, dedicated executable called `emu-worker`.

It would build against a pinned Azahar source revision and replace its normal frontend with a minimal custom one.

Its responsibilities:

| Capability                                                         | Implementation                           |
| ------------------------------------------------------------------ | ---------------------------------------- |
| Load `.3dsx`, `.3ds`, supported `.cci`/`.cxi` and installed titles | Existing Azahar loaders                  |
| Execute ARM11 code                                                 | Dynarmic                                 |
| Run operating-system services                                      | Azahar HLE                               |
| Emulate PICA200                                                    | Existing software/Vulkan implementations |
| Emulate audio                                                      | Existing DSP implementations             |
| Receive input                                                      | Custom frontend adapter                  |
| Output screens                                                     | Offscreen framebuffer                    |
| Receive debugger commands                                          | New internal command queue               |
| Emit structured events                                             | Instrumentation hooks                    |
| Handle storage                                                     | Isolated per-instance user directories   |

The worker should have no knowledge of GPUI, MCP, agents, or application layout.

### **Two possible routes**

**Route A: use Azahar's Libretro core**

Fastest integration experiment. Implement a minimal Rust Libretro host and obtain video, audio, input, and basic run control.

Its existing `retro_run()` interface provides a convenient execution boundary. [Source](https://github.com/azahar-emu/azahar/blob/master/src/citra_libretro/citra_libretro.cpp?utm_source=chatgpt.com)

However, Libretro doesn't provide all the internal debugging access we want. We'd eventually need additional hooks and control semantics.

**Route B: custom Azahar worker**

Build a minimal native frontend directly against the core, implementing `EmuWindow` and the other required frontend interfaces.

More initial integration work, but we can directly access the kernel, memory system, GPU command processing, and scheduler.

This is the route I'd use for the actual project.

Route A is useful as a disposable proof of concept. Route B has the architecture we'll ultimately need.

I'd also retain the unmodified Azahar frontend as a compatibility reference during development.

## **4. GPUI: building the desktop application**

GPUI is a reasonable choice here. Its entity and view model is well suited to a desktop debugger with multiple windows, long-lived panels, and independently updating data.

It is also still changing rapidly, so I'd pin the GPUI Git revision rather than follow `main` automatically. The current GPUI README explicitly documents its pre-1.0 status and breaking changes.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub



### **Proposed workspace**

Emulator Debugger

Instance 01

CPU / ARM11

PC 0x00102A40

SP 0x080FFFC0

GPU

P3D command stream

Shader / texture state

Execution

Breakpoints · watches

Threads · IPC · events

Event timeline

Illustrative layout

Conceptual GPUI workspace, not a running application.

The application should support detachable panels, multiple sessions, independent window layouts, and saved workspace configurations.

The emulator screens should remain independent of the UI's repaint rate.

### **The framebuffer integration problem**

This is an important implementation detail.

On Windows, GPUI uses a Direct3D-based renderer; Azahar's accelerated graphics use Vulkan or OpenGL. Their textures are not automatically interchangeable.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub+1



I'd implement screen presentation in two stages.

**Initially: shared-memory pixel buffers.**

The worker outputs the two screens into a small ring of framebuffers. GPUI reads the latest completed frame and displays it using its existing image support.

GPUI already has `RenderImage` and custom image sources that can accept supplied pixels.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub+1



At native resolution, two 32-bit screen buffers total roughly 675 KiB per frame. That's an acceptable starting point for a debugger, although asynchronous GPU readback still needs care.

Use a sequence number and completion signaling so GPUI never reads a partially written frame.

**Eventually: native GPU texture sharing.**

Implement a custom GPUI rendering extension that imports shared GPU resources, avoiding the CPU round trip.

There is relevant experimental work, including [gpui_interop](https://docs.rs/crate/gpui_interop/latest?utm_source=chatgpt.com), but it targets a separate GPUI-derived stack, so I would treat it as a design reference, not a guaranteed compatible dependency.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://docs.rs\&sz=32)

Docs.rs



For native-resolution debugging, the simple approach is probably sufficient. For multiple upscaled 3D instances, GPU sharing becomes much more valuable.

## **5. Make debugging the primary interface**

This is where your emulator should substantially improve on Citra.

Rather than building tools around the core after the fact, give the core a structured observation and control API from the beginning.

### **A single command interface**

The GPUI debugger, CLI, automated test runner, and MCP server should all use the same operations.

For example:

```
emu.create
emu.load
emu.run
emu.pause
emu.reset
emu.step_instruction
emu.step_frame
emu.run_until

cpu.registers
cpu.disassemble
cpu.breakpoint_set

memory.read
memory.write
memory.watch

gpu.capture_frame
gpu.capture_draw
gpu.registers
gpu.shader_dump

kernel.threads
kernel.processes
kernel.ipc_trace

trace.start
trace.stop
trace.export
```

These are proposed API names, not existing Azahar commands.

I'd also expose memory mappings, process handles, GPU resource ownership, VRAM texture decoding, shader state, service-call arguments, and the virtual clock.

### **Correctness of debugger access**

An agent reading memory should not race with the emulation thread.

The current Azahar build enables synchronized scripting requests by default, following work on a previously reported race where RPC reads of GPU-backed memory could crash the emulator. The issue was resolved in October 2026.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub



I'd make this a stronger architectural guarantee: any operation that accesses mutable emulated state must execute at a defined safe point.

Use two categories of reads:

| Read                | Guarantee                                                         |
| ------------------- | ----------------------------------------------------------------- |
| Live telemetry      | Fast, potentially slightly stale, marked with a timestamp         |
| Consistent snapshot | CPU/GPU state is synchronized before reading                      |
| Memory mutation     | Executed at a safe point, with cache/JIT invalidation as required |
| Full capture        | Waits for pending operations needed to produce a coherent capture |

For example, a framebuffer read that requires a Vulkan download might need coordination with a rendering thread. Pausing CPU execution alone may not be sufficient.

### **Tracing without destroying performance**

I would avoid permanently logging every instruction or GPU register write.

Instead, implement a bounded, structured event system with selectable capture levels.

A typical GPU event could be:

```
{
  "instance": "emu-01",
  "sequence": 18422,
  "frame": 730,
  "guest_tick": 126502000,
  "category": "gpu.draw",
  "command_address": "0x14002000",
  "vertices": 384,
  "shader_id": "8d31..."
}
```

The tick is a virtual emulation timestamp; it should not be presented as accurately calibrated hardware timing.

Each capture should preserve its emulator revision, backend, configuration, game build, and instrumentation settings.

I'd separate **guest execution events**, **emulator host overhead**, and **estimated hardware costs**. Mixing those produces misleading performance profiles.

## **6. MCP and agentic debugging**

Build the MCP server in Rust using the official RMCP library.

It already supports tools, resources, prompts, subscriptions, and the usual local and HTTP transports.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://github.com\&sz=32)

GitHub



The MCP server should communicate with the Rust session manager, not directly with C++ internals.

This gives an agent the same capabilities as the debugger UI.

### **The most useful workflows**

**Crash investigation**

The agent loads a test program, reproduces the fault, captures registers and a bounded memory region, resolves the instruction against an ELF, and reports the faulting code path.

**Graphics investigation**

Capture one frame's command lists, inspect the draw sequence, decode textures, identify the shaders and render targets, and compare output against a known-good frame.

**Regression comparison**

Run two emulator versions or two game builds under identical input, then find the first differing CPU checkpoint, GPU command, framebuffer, or service response.

**Hardware research**

Execute a benchmark on the emulator, compare its results with physical-console data, and identify which hardware timing assumptions need investigation.

Agents should be able to perform long, scripted sequences through a single tool call, instead of making thousands of tiny calls.

For example:

```
{
  "instance": "emu-01",
  "scenario": "goldenrod_entry",
  "stop": {
    "frame_count": 600
  },
  "capture": [
    "frame_times",
    "gpu_draws",
    "cpu_sampling",
    "screenshots_on_anomaly"
  ]
}
```

The test runner would own input during the scenario, with live keyboard input disabled. Every run gets its own process, storage root, logs, and artifact directory.

Also enforce limits on trace size, execution duration, and memory reads. A poorly written agent loop shouldn't be able to exhaust the host's RAM.

## **7. Multiple emulator instances**

This should be a first-class feature, not something achieved by launching the same executable several times manually.

The Rust session manager would own worker process lifetimes and their resources.

| Resource             | Isolation                               |
| -------------------- | --------------------------------------- |
| CPU and kernel state | One worker process per instance         |
| GPU state            | Independent renderer and command queues |
| Memory               | Independent address spaces              |
| NAND and SD          | Private directories by default          |
| Saves                | Private copy-on-write overlays          |
| Debugger             | Instance-specific session               |
| RPC endpoint         | Unique named pipe or local socket       |
| Input                | Explicit routing per instance           |
| Logs and captures    | Unique run identifiers                  |

For communication, I'd use versioned Protocol Buffers over local named pipes on Windows and Unix sockets elsewhere. Large framebuffers and captures should travel through shared memory rather than the control channel.

A useful feature would be *clone from snapshot*: start one instance from an established save state, duplicate it, and run several experiments in parallel.

We should not assume Azahar's existing save states are universally reliable or portable across backend revisions. Validate snapshot compatibility and version it explicitly.

I'd also support batch execution without GPUI:

```
emulator-cli run `
  --rom test.3dsx `
  --instances 4 `
  --scenario tests/gpu-commands.yaml `
  --output results/
```

A GUI-free runner allows automated regression testing and agent-controlled experimentation without rendering any desktop windows.

## **8. Evolving toward hardware accuracy**

This is the part that determines whether the project eventually becomes something fundamentally different from Citra.

Don't simply add an `accurate=true` flag to Azahar.

Instead, gradually introduce independent models around clearly defined emulated hardware boundaries.

| Subsystem  | Bootstrap implementation         | Future implementation                               |
| ---------- | -------------------------------- | --------------------------------------------------- |
| ARM11      | Dynarmic                         | Rust interpreter with cache/timing model            |
| ARM9       | Azahar's existing approximations | Independent ARM946 model and firmware execution     |
| Kernel     | Azahar HLE                       | More accurate HLE, eventually real-kernel execution |
| PICA200    | Azahar software/Vulkan renderer  | Reference renderer plus measured timing model       |
| Memory     | Azahar memory system             | Explicit buses, cache visibility, and contention    |
| DSP        | Teakra                           | Retain and improve LLE/timing                       |
| Display    | Azahar framebuffer/VBlank model  | More accurate scanout and event timing              |
| Scheduling | Azahar CoreTiming                | Deterministic hardware-event scheduler              |

The challenge is that Azahar doesn't expose clean, hot-swappable interfaces for all of these components today.

Consequently, **the first worker/backend boundary is a practical isolation layer, not yet a complete hardware-component plugin architecture.**

Within the Azahar-derived worker, I'd initially add instrumentation and narrow interfaces around specific components. Only replace them once the new implementation has independent tests.

### **Accuracy as a measurable property**

Every new hardware model should have three forms of evidence:

| Evidence                         | Purpose                                             |
| -------------------------------- | --------------------------------------------------- |
| Coffeepot / 3dbrew documentation | Establish expected behavior and known uncertainties |
| Physical 3DS measurements        | Establish what the hardware actually does           |
| Cross-emulator tests             | Detect regressions against existing implementations |

Coffeepot already provides useful building blocks here, particularly its [hardware testing](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/testing-on-hardware.md?utm_source=chatgpt.com), [debugging](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/engineering/debugging.md?utm_source=chatgpt.com), and [PICA200](https://github.com/estyseesghosts/coffeepot/blob/main/ctr/hardware/gpu.md?utm_source=chatgpt.com) material.

Its testing guide explicitly distinguishes hardware results from emulator-only tests, records measurement overhead, and emphasizes proving that a tested path actually executed. Those principles should carry directly into the emulator's validation harness.

I'd make it possible to record evidence for every modeled hardware behavior, with an explicit confidence level: documented, measured, inferred, or unknown.

### **The first major independent component: PICA200**

After the debugger is working, I'd begin with the PICA200 rather than a new ARM11 interpreter.

Reasons: Azahar's graphics infrastructure can provide an execution reference; Coffeepot and 3dbrew have extensive GPU documentation; and the GPU is one of the largest obstacles to trustworthy original-3DS performance estimates.

3dbrew even documents the PICA200's hardware hotspot registers, including eight counters associated with different pipeline activities, though their interpretations remain partially reverse-engineered.&#x20;

[image](https://www.google.com/s2/favicons?domain=https://www.3dbrew.org\&sz=32)

3dbrew



A new GPU implementation could initially share Azahar's command decoder, then introduce independent execution, rasterization, and timing modules.

That gives us an opportunity to establish an actual accuracy improvement before attempting an entire system rewrite.

## **9. Proposed repository structure**

I would create a new repository rather than maintain this as a conventional Azahar frontend fork.

```
emulator/
├── Cargo.toml
├── rust-toolchain.toml
├── AGENTS.md
│
├── crates/
│   ├── desktop/          # GPUI application
│   ├── session/          # Worker lifecycle, isolation
│   ├── protocol/         # IPC schemas and generated types
│   ├── mcp/              # Agent interfaces
│   ├── debugger/         # CPU/GPU/kernel debugging
│   ├── trace/            # Structured captures, timelines
│   ├── runner/           # Automated test scenarios
│   ├── hardware/         # Hardware model interfaces
│   └── accuracy/         # Differential tests, comparisons
│
├── workers/
│   ├── azahar/           # C++ worker adapter and patches
│   └── native/           # Future Rust emulation backend
│
├── vendor/
│   └── azahar/           # Pinned source revision
│
├── tools/
│   └── hardware-tests/   # devkitPro 3DS test programs
│
├── tests/
│   ├── smoke/
│   ├── integration/
│   ├── differential/
│   └── fixtures/
│
└── docs/
    ├── architecture/
    ├── emulation/
    ├── debugging/
    ├── accuracy/
    └── research/
```

`crates/hardware` would initially define Rust types and interfaces, not pretend to contain a complete emulator.

Likewise, `workers/native` remains an experimental target until it can execute an independently useful subset of 3DS hardware.

### **Licensing**

One nontrivial constraint is that Azahar's emulation code contains GPL-licensed components. Modified worker code must comply with the applicable licenses, and separation into processes does not automatically eliminate combined-work questions.

For the simplest bootstrap and redistribution path, I'd make the project GPLv3-compatible, retain original notices, identify bundled dependency licenses, and avoid bundling Nintendo firmware, keys, or proprietary game data.

## **10. Implementation milestones**

01

**Bootstrap a running worker**

Pin Azahar, build it from the new repository, replace its frontend, load a `.3dsx`, and produce video frames through shared memory.

Exit criterion: the same program runs in stock Azahar and the custom worker with matching expected output.

02

**Build the GPUI application**

Display both screens, implement controller/touch input, loading, pause/resume, and basic emulation settings.

Exit criterion: the original Azahar GUI is no longer required to play or debug the supported test programs.

03

**Multi-instance and CLI**

Launch independent workers, give each separate storage, and manage them through the Rust session service.

Exit criterion: four concurrent instances can run different workloads without save corruption, configuration collisions, or shared debugger state.

04

**Integrated debugger and MCP**

Implement synchronized memory access, breakpoints, registers, thread inspection, frame captures, and structured RPC.

Exit criterion: an agent can reproduce and investigate a crash without manual emulator interaction.

05

**GPU and performance instrumentation**

Add per-draw captures, command inspection, shader inspection, event timelines, and repeatable workload benchmarks.

Exit criterion: two runs can be compared to find their earliest meaningful execution difference.

06

**First independent hardware model**

Build the PICA200 accuracy test suite, implement a narrow independent GPU component, and validate it against actual hardware.

Exit criterion: demonstrably better behavior on a documented set of cases where Azahar differs from the physical 3DS.

## **11. The important architectural rules**

The guiding principle is to **protect the new emulator from becoming permanently dependent on Azahar's internal design**.

That means keeping the Rust application independent of C++ implementation details, defining stable control and capture formats, and identifying the eventual hardware boundaries early.

It does *not* mean prematurely abstracting every Citra subsystem or trying to make everything interchangeable before there's a second implementation.

I'd also keep correctness and performance research separate. An accurate CPU interpreter, for example, might initially be slower than Dynarmic. That's acceptable for a research backend, provided the fast backend remains usable.

The most important capabilities to preserve from the beginning are independent instances, repeatable execution, well-defined state snapshots, structured instrumentation, and headless operation.

Those have lasting value regardless of how much of Azahar eventually gets replaced.

## **Where I'd start**

I would begin with **a custom Azahar worker, a Rust process/session manager, and the smallest possible GPUI application that can load and display a 3DS program**.

No custom CPU or GPU yet. No attempt to reimplement Nintendo's operating system.

After that, add the debugging interface and MCP before expanding the emulator itself.

This gives you two valuable things:

**Short term:** a 3DS emulator purpose-built for agentic debugging, parallel testing, instrumentation, and your development workflow.

**Long term:** a platform in which progressively more accurate hardware implementations can replace Citra-derived functionality, with a real-hardware validation system already established.

The immediate development risk isn't the difficulty of emulating the PICA200. It's accidentally making the new platform so tightly coupled to Azahar that the later hardware rewrite requires rebuilding the debugger and tooling too.

If we get that boundary right, the bootstrap work isn't disposable. It becomes the foundation of the eventual accuracy-focused emulator.
