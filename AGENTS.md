# Implementer rules — read before touching code

## Writing Style Rules

These rules apply to both chat responses, and to written documentation.

- **Be direct.** Answer the question immediately. No introductions, filler, or unnecessary conclusions.
- **Use natural language.** Write like a competent engineer, not a marketer, consultant, or chatbot.
- **Be concise, not shallow.** Keep explanations short where possible, but never sacrifice technical detail, accuracy, or important nuance.
- **Avoid AI-sounding prose.** No artificial contrasts, dramatic fragments, forced profundity, rhetorical questions, or rules of three.
- **Don't flatter or validate.** Skip praise, excessive agreement, performative empathy, and conversational pleasantries.
- **Be specific.** Prefer concrete facts, mechanisms, measurements, examples, and actionable steps over vague abstractions.
- **Don't overstructure.** Use headings, lists, and tables only when they genuinely improve readability.
- **Be honest about uncertainty.** Distinguish verified facts, reasonable inferences, and speculation. Don't manufacture confidence.
- **Think critically.** Challenge incorrect assumptions, identify tradeoffs, and explain disagreements without hedging unnecessarily.
- **Prioritize substance.** Every sentence should contribute useful information. If removing it changes nothing, remove it.

## Truth and Citations 

You are encouraged to come up with creative solutions for the obscure and arcane problems you might face while working in this folder. This does not mean you just throw shit at the wall to see what sticks. Every decision you make, every change you write, you must be able to justify that decision against a citation - either a line from a source file, the project documentation provided to you, or otherwise. Do not pull shit out of your ass.

## Half Steps 

Having a task's deliverables fully achieved on paper does not always mean those deliverables were completed correctly. Something like, being asked to complete a port, and outputting a technically complete port - that brute forces rendering in software, and as such runs terribly on real hardware. That's awful, benefits nobody, and wastes the users time and money. Do not find the minimum possible way to complete a deliverable if it means compromising on that deliverable, or giving the user something similar to -   but not exactly what they asked for.

## Order of authority

1. Measured physical-hardware behavior > independent hardware trace > documented specification > existing emulator behavior > hypothesis.
2. User's explicit decisions: native Azahar integration (**not Libretro**), Rust + GPUI, multiple isolated workers, two Azahar branches (`integration`, `experimental`).
3. Preserve compatibility and successful baselines before speculative accuracy changes.

## Attribution

Agents never insert their own branding as a commit attribution or co-attribution. All commits are authored solely by the git identity already set on the authors computer. For example, the project owner's git identity should always be `terezi <jules@foxtails.me>`.

## Documentation 

Agents should keep documentation and comments as brief as possible. This does not mean spare on technical details. Explain what it is, what it's doing, how it works, what it isn't, and the relevant necessary information. 

Agents must keep documentation consistent.

Agents must always be able to refer back to primary sources (such as 3DBrew, the citra wiki, source code citations from azahar) to justify changes or decisions. Agents are encouraged to use secondary sources, such as Coffeepot, for easy lookup of information - coffeepot, however, should not be your citation. 

## Branch discipline

- `integration`: only smallest viable native worker/front-end hooks, configurable build target, deterministic control queue, frame/audio/input adapters, and proven bug fixes required for integration. No GPU timing redesign or pervasive tracing. Regular upstream merges.
- `experimental`: based on `integration`; structured tracing, debugger controls, snapshot experiments, timing models, hardware-fidelity experiments. Never merge `experimental` wholesale into `integration`.
- Any item that seems essential to integration but introduces accuracy behavior changes needs an ADR and separate two-step PR.
- Freeze upstream baseline SHA and CMake options in each test report. One independent worktree/build directory per branch. Never build both using one configured CMake cache.

## Evidence and debugging

- Never confuse host milliseconds, virtual 3DS ticks, and empirically inferred hardware cycles.
- Never claim a new accuracy feature without a hardware witness, an independent reference, and a counterexample where the old behavior differed.
- Any cross-thread mutable emulated-state read MUST run at a defined safe point or return a labeled approximate telemetry snapshot.
- Instrument with bounded buffers, severity filters and loss counters. Avoid `printf` in hot paths. Measure instrumentation-on/off overhead.
- A GPU draw capture must describe the actual command/state at the draw, not a post-frame memory reconstruction. Pre-draw hashes must be computed by the draw or use frozen memory snapshots; never hash unrelated time-dependent registers as image identity.
- Label examples explicitly: **verified compiled**, **verified hardware**, **verified emulator**, **source inspected**, or **proposed**. No unverifiable green check marks.

## Implementation quality

- Prefer a narrow PR with a runnable acceptance test over a broad refactor.
- Keep the C++ worker free of GPUI, MCP, and agent concepts.
- Keep FFI structs plain, fixed-width and versioned; do not expose `std::string`, `std::vector`, `Core::System*` or internal layouts across IPC.
- No unsandboxed remote RPC listener by default. MCP/control access local-only and scoped to explicitly created workers.
- Treat ROMs, firmware, save data, logs and captures as sensitive user assets. Do not upload them, bundle them, or include them in bug reports without consent.
- On worker crash, retain logs and metadata, invalidate frame-buffer leases, terminate child descendants and release per-instance locks.
- Do not implement experimental multi-instance support by embedding two Azahar `Core::System`s in one process: globals and lifetime assumptions are not cleared.
- Avoid speculative architectural abstractions that lack a second backend or a concrete test.

## Definition of done for every PR

1. Explain touched subsystem, expected change, and branch rationale.
2. Enumerate upstream source revision and build flags; tests and known untested platforms.
3. Include a test that would fail without the change (or justify an instrumentation-only PR).
4. Record guest output hash/checkpoints and host overhead for performance-relevant changes.
5. Confirm clean shutdown, second launch, and two parallel workers if shared process/storage infrastructure changed.
6. Update docs/source-map and protocol schema for altered interfaces.
7. Make limitations explicit; retain rollback flag for risk-bearing work.
