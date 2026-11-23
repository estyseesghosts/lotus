/* Owned marker fixture (lotus#83). Built with -DLOTUS_MARKERS it emits guest/lotus_markers.h
 * records; without it the same program emits none and contains no marker code.
 *
 * Main thread: identify, then 60 paced iterations of
 *   begin frame, begin update, counter frame_index, end update, instant tick, end frame.
 * Iteration 30 adds a burst of 600 instants (`burst`), more than the worker keeps in one
 * frame; the worker also drops that frame's later records and counts them. Iteration 40
 * opens 33 nested `deep` scopes inside the open `frame` scope and closes them: with the
 * process-wide limit of LOTUS_MARKER_MAX_DEPTH (32) open scopes, the guest drops two begins
 * and their two ends and reports 4 in the next record. A second thread emits 30 `child_work` scopes with a `child_tick` instant.
 * `stage` is 2 when the main loop is done and `child_done` 1 when the thread is. */
#ifdef LOTUS_MARKERS
#define LOTUS_MARKERS_IMPLEMENTATION
#endif
#include "../../guest/lotus_markers.h"

typedef unsigned int u32;
volatile u32 stage = 1, iterations, child_done;
static unsigned char child_stack[0x1000] __attribute__((aligned(8)));

static void sleep_ms(u32 ms) {
    unsigned long long ns = (unsigned long long)ms * 1000000ull;
    register u32 r0 __asm__("r0") = (u32)ns, r1 __asm__("r1") = (u32)(ns >> 32);
    __asm__ volatile("svc 0x0a" : "+r"(r0), "+r"(r1)::"r2", "r3", "memory");
}

/* svcCreateThread: r0 priority, r1 entry, r2 argument, r3 stack top, r4 processor
 * (Azahar src/core/hle/kernel/svc.cpp CreateThread). */
static void create_thread(void (*entry)(void), u32 core) {
    register u32 r0 __asm__("r0") = 0x30, r1 __asm__("r1") = (u32)entry, r2 __asm__("r2") = 0,
                 r3 __asm__("r3") = (u32)(child_stack + sizeof(child_stack)),
                 r4 __asm__("r4") = core;
    __asm__ volatile("svc 0x08" : "+r"(r0), "+r"(r1), "+r"(r2), "+r"(r3), "+r"(r4)::"memory");
}

static void child(void) {
    for (u32 n = 0; n < 30; ++n) {
        LOTUS_MARKER_BEGIN_SCOPE("child_work");
        LOTUS_MARKER_INSTANT_EVENT("child_tick", n);
        LOTUS_MARKER_END_SCOPE("child_work");
        sleep_ms(16);
    }
    child_done = 1;
    __asm__ volatile("svc 0x09" ::: "memory"); /* svcExitThread */
    for (;;) {
    }
}

__attribute__((noinline)) int main(void) {
    LOTUS_MARKER_IDENTIFY_BUILD("lotus-fixture-markers-1");
    create_thread(child, 0);
    for (u32 frame = 0; frame < 60; ++frame) {
        LOTUS_MARKER_BEGIN_SCOPE("frame");
        LOTUS_MARKER_BEGIN_SCOPE("update");
        LOTUS_MARKER_COUNTER_VALUE("frame_index", frame);
        LOTUS_MARKER_END_SCOPE("update");
        if (frame == 30)
            for (u32 n = 0; n < 600; ++n)
                LOTUS_MARKER_INSTANT_EVENT("burst", n);
        if (frame == 40) {
            for (u32 depth = 0; depth < 33; ++depth)
                LOTUS_MARKER_BEGIN_SCOPE("deep");
            for (u32 depth = 0; depth < 33; ++depth)
                LOTUS_MARKER_END_SCOPE("deep");
        }
        LOTUS_MARKER_INSTANT_EVENT("tick", frame);
        LOTUS_MARKER_END_SCOPE("frame");
        iterations = frame + 1;
        sleep_ms(16);
    }
    stage = 2;
    for (;;)
        sleep_ms(1000);
}

__attribute__((naked, section(".text.start"), used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
