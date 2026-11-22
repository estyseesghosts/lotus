// Freestanding C++ source-debugging fixture (lotus#77): ARM code calling Thumb code
// (BLX), a class method and an always_inline helper, built with -O0 -gdwarf-5 so every
// statement keeps its own line rows. Native tests find lines by their `// @name`
// markers; keep one statement per marked line. No firmware or host libraries.
typedef unsigned int u32;

// Read-only data, so the 3DSX carries a .rodata segment like real homebrew.
extern "C" __attribute__((used)) const char fixture_name[] = "lotus-source-fixture";
volatile u32 frames;
volatile u32 total;

#ifdef LOTUS_LAYOUT_SHIFT
// Rebuild variant: moves later functions without changing any line number.
__attribute__((noinline, used)) u32 layout_shift(u32 x) { return x * 7 + 3; }
#endif

__attribute__((always_inline)) static inline u32 scaled(u32 x) {
    u32 doubled = x * 2; // @inline_body
    return doubled + 1;
}

class Accumulator {
public:
    u32 sum;
    __attribute__((noinline)) void add(u32 value) {
        sum = sum + value; // @method_body
    }
};

// Constant-initialised into .data: no constructor runs before main.
static Accumulator accumulator = {5};

__attribute__((noinline, target("thumb"))) u32 thumb_square(u32 x) {
    u32 squared = x * x; // @thumb_body
    return squared;
}

__attribute__((noinline)) void frame() {
    u32 base = frames + 1;           // @frame_first
    u32 square = thumb_square(base); // @frame_call_thumb
    accumulator.add(square);         // @frame_call_method
    u32 adjusted = scaled(square);   // @frame_inline
    total = adjusted + accumulator.sum; // @frame_store
    frames = base;                   // @frame_last
}

extern "C" __attribute__((noinline)) int main() {
    for (;;) frame(); // @main_loop
}

extern "C" __attribute__((naked, section(".text.start"), used)) void _start() {
    __asm__ volatile("bl main\n1: b 1b");
}
