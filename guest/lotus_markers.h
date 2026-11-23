/* Lotus guest debug markers, ABI version 1 (lotus#83).
 *
 * Optional instrumentation for 3DS homebrew. Define LOTUS_MARKERS before including this
 * header to emit records, and LOTUS_MARKERS_IMPLEMENTATION as well in exactly one source
 * file, which then owns the counters. Without LOTUS_MARKERS every macro expands to nothing
 * and its arguments are not evaluated, so a release build contains no marker code, calls
 * or data, and needs no Lotus at run time.
 *
 * Transport: one svcOutputDebugString (SVC 0x3D) per record, made on the calling thread.
 * A retail console does nothing with it (Azahar src/core/hle/kernel/svc.cpp
 * OutputDebugString). AZAHAR logs it at debug level and otherwise ignores it. The CITnLO
 * worker decodes it at the call, so records keep guest order and the kernel thread and
 * process that made them. There is no guest-side buffer: a record is either delivered or
 * counted in the next record's `dropped`.
 *
 * Record layout, little-endian, 40-byte header then the name:
 *   0  char[4] magic "LTMK"
 *   4  u8      version, 1
 *   5  u8      type: 1 scope begin, 2 scope end, 3 instant, 4 counter, 5 identify
 *   6  u16     name length, 1..64 printable ASCII (0x20..0x7E)
 *   8  u32     sequence, per process, incremented for every record built
 *  12  u32     records this process dropped since the previous delivered record
 *  16  u32     name id: FNV-1a 32 of the name
 *  20  u32     reserved, 0
 *  24  s64     value: counter value, instant payload, else 0
 *  32  u64     guest tick from svcGetSystemTick (SVC 0x28), guest-provided
 *  40  char[n] name; for identify, the build identity
 *
 * The guest tick is what the program read, not a measurement of PICA or host time.
 * Open scopes are counted process-wide up to LOTUS_MARKER_MAX_DEPTH; deeper begins and
 * their ends are dropped and counted. The worker pairs begins and ends per kernel thread.
 * The sequence, drop and depth counters are plain integers without locking, so threads
 * that emit concurrently can repeat a sequence number or miscount depth.
 */
#ifndef LOTUS_MARKERS_H
#define LOTUS_MARKERS_H

#ifdef LOTUS_MARKERS

#define LOTUS_MARKER_VERSION 1
#define LOTUS_MARKER_MAX_NAME 64
#define LOTUS_MARKER_MAX_DEPTH 32

enum {
    LOTUS_MARKER_BEGIN = 1,
    LOTUS_MARKER_END = 2,
    LOTUS_MARKER_INSTANT = 3,
    LOTUS_MARKER_COUNTER = 4,
    LOTUS_MARKER_IDENTIFY = 5,
};

typedef struct {
    unsigned int sequence;
    unsigned int dropped;
    unsigned int depth;
} lotus_marker_state;

#ifdef LOTUS_MARKERS_IMPLEMENTATION
lotus_marker_state lotus_marker_globals;
#else
extern lotus_marker_state lotus_marker_globals;
#endif

static inline unsigned long long lotus_marker_tick(void) {
    register unsigned int lo __asm__("r0");
    register unsigned int hi __asm__("r1");
    __asm__ volatile("svc 0x28" : "=r"(lo), "=r"(hi) :: "r2", "r3", "memory");
    return ((unsigned long long)hi << 32) | lo;
}

static inline void lotus_marker_put32(unsigned char* out, unsigned int value) {
    out[0] = (unsigned char)value;
    out[1] = (unsigned char)(value >> 8);
    out[2] = (unsigned char)(value >> 16);
    out[3] = (unsigned char)(value >> 24);
}

static inline void lotus_marker_emit(unsigned int type, const char* name, long long value) {
    unsigned char record[40 + LOTUS_MARKER_MAX_NAME];
    unsigned int length = 0, hash = 2166136261u, i;
    unsigned long long tick;
    lotus_marker_state* state = &lotus_marker_globals;
    ++state->sequence;
    if (type == LOTUS_MARKER_BEGIN && state->depth >= LOTUS_MARKER_MAX_DEPTH) {
        ++state->depth; /* its end is dropped too */
        ++state->dropped;
        return;
    }
    if (type == LOTUS_MARKER_END) {
        if (state->depth == 0 || state->depth-- > LOTUS_MARKER_MAX_DEPTH) {
            ++state->dropped;
            return;
        }
    }
    while (name[length] && length < LOTUS_MARKER_MAX_NAME) {
        const unsigned char c = (unsigned char)name[length];
        record[40 + length] = (c >= 0x20 && c <= 0x7e) ? c : '?';
        hash = (hash ^ record[40 + length]) * 16777619u;
        ++length;
    }
    if (length == 0) {
        ++state->dropped;
        return;
    }
    tick = lotus_marker_tick();
    record[0] = 'L';
    record[1] = 'T';
    record[2] = 'M';
    record[3] = 'K';
    record[4] = LOTUS_MARKER_VERSION;
    record[5] = (unsigned char)type;
    record[6] = (unsigned char)length;
    record[7] = 0;
    lotus_marker_put32(record + 8, state->sequence);
    lotus_marker_put32(record + 12, state->dropped);
    lotus_marker_put32(record + 16, hash);
    lotus_marker_put32(record + 20, 0);
    lotus_marker_put32(record + 24, (unsigned int)value);
    lotus_marker_put32(record + 28, (unsigned int)((unsigned long long)value >> 32));
    lotus_marker_put32(record + 32, (unsigned int)tick);
    lotus_marker_put32(record + 36, (unsigned int)(tick >> 32));
    state->dropped = 0;
    if (type == LOTUS_MARKER_BEGIN)
        ++state->depth;
    {
        register const unsigned char* r0 __asm__("r0") = record;
        register unsigned int r1 __asm__("r1") = 40 + length;
        __asm__ volatile("svc 0x3d" : "+r"(r0), "+r"(r1) :: "r2", "r3", "memory");
    }
}

#define LOTUS_MARKER_IDENTIFY_BUILD(build) lotus_marker_emit(LOTUS_MARKER_IDENTIFY, (build), 0)
#define LOTUS_MARKER_BEGIN_SCOPE(name) lotus_marker_emit(LOTUS_MARKER_BEGIN, (name), 0)
#define LOTUS_MARKER_END_SCOPE(name) lotus_marker_emit(LOTUS_MARKER_END, (name), 0)
#define LOTUS_MARKER_INSTANT_EVENT(name, value) \
    lotus_marker_emit(LOTUS_MARKER_INSTANT, (name), (long long)(value))
#define LOTUS_MARKER_COUNTER_VALUE(name, value) \
    lotus_marker_emit(LOTUS_MARKER_COUNTER, (name), (long long)(value))

#else

#define LOTUS_MARKER_IDENTIFY_BUILD(build) ((void)0)
#define LOTUS_MARKER_BEGIN_SCOPE(name) ((void)0)
#define LOTUS_MARKER_END_SCOPE(name) ((void)0)
#define LOTUS_MARKER_INSTANT_EVENT(name, value) ((void)0)
#define LOTUS_MARKER_COUNTER_VALUE(name, value) ((void)0)

#endif
#endif
