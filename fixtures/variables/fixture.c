// Freestanding C variables fixture (lotus#78), built -O1 -gdwarf-5 so locals live in
// registers, on the stack, in location lists and in inlined scopes. Native tests stop at
// `// @name` markers with source breakpoints and compare variables with the values this
// file defines. No firmware or host libraries.
typedef unsigned int u32;
typedef signed short s16;

__attribute__((used)) const char fixture_name[] = "lotus-variables-fixture";

enum mode { MODE_IDLE = 0, MODE_RUN = 2, MODE_HALT = 7 };

struct flags {
    u32 ready : 1;
    u32 level : 3;
    int delta : 5;
    u32 tail : 7;
};
struct point {
    s16 x;
    s16 y;
};
struct node {
    u32 id;
    struct node *next;
};
union word {
    u32 raw;
    unsigned char bytes[4];
};
struct record {
    const char *label;
    struct point origin;
    struct point path[3];
    struct flags flags;
    enum mode mode;
    union word word;
    struct node *ring;
    u32 *missing;
    u32 (*hook)(u32);
    long long big;
};

volatile u32 counter;
volatile u32 sink;

__attribute__((noinline)) u32 hook(u32 x) { return x + 1; }

// A two-node cycle, and a pointer into the unmapped first page.
static struct node ring_a;
static struct node ring_b = {2, &ring_a};
static struct node ring_a = {1, &ring_b};
struct record record = {
    "probe", {-3, 4}, {{1, 2}, {3, 4}, {5, 6}}, {1, 5, -6, 100}, MODE_RUN,
    {0x11223344u}, &ring_a, (u32 *)0x10, hook, -1234567890123LL,
};

// AAPCS passes the fifth and later word arguments on the stack.
__attribute__((noinline)) u32 six(u32 a, u32 b, u32 c, u32 d, u32 e, u32 f) {
    sink = a + b + c + d;
    sink = e * f; // @stack_args
    return e + f;
}

__attribute__((noinline, target("thumb"))) u32 thumb_sum(const struct point *path, u32 count) {
    u32 total = 0;
    for (u32 i = 0; i < count; i++)
        total += (u32)path[i].x; // @thumb_loop
    sink = total; // @thumb_body
    return total;
}

__attribute__((always_inline)) static inline u32 mix(const struct point *p, u32 k) {
    u32 local = (u32)p->x * k + (u32)p->y;
    sink = local; // @inline_body
    return local;
}

__attribute__((noinline)) u32 probe(struct record *r) {
    u32 lost = counter; // a volatile load whose value is never used again
    u32 kept = counter + 40;
    u32 mixed = mix(&r->origin, kept);
    u32 summed = thumb_sum(r->path, 3);
    sink = six(kept, 2, 3, 4, 5, 6);
    sink = mixed + kept + summed; // @probe_after
    return kept;
}

__attribute__((noinline)) u32 descend(struct record *r, u32 depth) {
    if (depth == 0)
        return probe(r) + 1;
    u32 below = descend(r, depth - 1);
    sink = depth; // a store after the call keeps the recursion: no tail-recursion elimination
    return below + depth; // @descend_return
}

__attribute__((noinline)) int main() {
    for (;;) {
        counter = counter + 1;
        sink = descend(&record, 2); // @main_call
    }
}

__attribute__((naked, section(".text.start"), used)) void _start() {
    __asm__ volatile("bl main\n1: b 1b");
}
