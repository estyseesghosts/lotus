/* Owned HID input fixture for recorded tests (lotus#95). It maps HID shared memory
 * (hid:USER GetIPCHandles, libctru source/services/hid.c) and folds every pad/touch entry
 * the HID module writes into `checksum`, in order. Layout: Azahar
 * src/core/hle/service/hid/hid.h SharedMem: pad.index at 0x10, pad entries (16 bytes,
 * current state first) at 0x28, touch.index at 0xB8, touch entries (x, y, valid) at 0xC8.
 * The module writes both at 234 Hz (pad_update_ticks), so `checksum` at a frame depends on
 * which HID update each input change reached: a replay that applies an input one frame
 * late changes it. The first 64 state changes are kept in `changes` as
 * {update, buttons, touch}. */
typedef unsigned int u32;
typedef unsigned short u16;
volatile u32 stage = 1, failure = 0, failed_stage = 0;
volatile u32 updates, transitions, checksum = 2166136261u, buttons, touch;
volatile u32 changes[64][3];
#define HID_MEMORY 0x10000000u
static volatile u32* command(void) {
    u32 tls;
    __asm__ volatile("mrc p15, 0, %0, c13, c0, 3" : "=r"(tls));
    return (volatile u32*)(tls + 0x80);
}
static void panic(u32 code) {
    failure = code;
    failed_stage = stage;
    register u32 r0 __asm__("r0") = 0;
    __asm__ volatile("svc 0x3c" : "+r"(r0)::"memory");
    for (;;) {
    }
}
static void send(u32 handle) {
    register u32 r0 __asm__("r0") = handle;
    __asm__ volatile("svc 0x32" : "+r"(r0)::"r1", "r2", "r3", "memory");
    if (r0 || command()[1])
        panic(r0 ? r0 : command()[1]);
}
static u32 connect(void) {
    register u32 r0 __asm__("r0") = 0;
    register const char* r1 __asm__("r1") = "srv:";
    __asm__ volatile("svc 0x2d" : "+r"(r0), "+r"(r1)::"r2", "r3", "memory");
    if (r0)
        panic(r0);
    return (u32)r1;
}
static u32 service(u32 srv, const char* name) {
    volatile u32* c = command();
    u32 length = 0;
    c[0] = 0x50100;
    c[1] = 0;
    c[2] = 0;
    while (name[length]) {
        ((volatile char*)&c[1])[length] = name[length];
        ++length;
    }
    c[3] = length;
    c[4] = 0;
    send(srv);
    return c[3];
}
static void map(u32 handle, u32 address) {
    register u32 r0 __asm__("r0") = handle, r1 __asm__("r1") = address, r2 __asm__("r2") = 1,
                 r3 __asm__("r3") = 0x10000000;
    __asm__ volatile("svc 0x1f" : "+r"(r0), "+r"(r1), "+r"(r2), "+r"(r3)::"memory");
    if (r0)
        panic(r0);
}
static void sleep_ms(u32 ms) {
    unsigned long long ns = (unsigned long long)ms * 1000000ull;
    register u32 r0 __asm__("r0") = (u32)ns, r1 __asm__("r1") = (u32)(ns >> 32);
    __asm__ volatile("svc 0x0a" : "+r"(r0), "+r"(r1)::"r2", "r3", "memory");
}
static u32 fold(u32 hash, u32 value) {
    for (u32 byte = 0; byte < 4; ++byte) {
        hash ^= (value >> (8 * byte)) & 0xff;
        hash *= 16777619u;
    }
    return hash;
}
__attribute__((noinline)) int main(void) {
    volatile u32* c = command();
    u32 srv = connect();
    c[0] = 0x10002; /* srv:RegisterClient */
    c[1] = 0x20;
    c[2] = 0;
    send(srv);
    u32 hid = service(srv, "hid:USER");
    c[0] = 0xa0000; /* GetIPCHandles: c[3] is the shared memory handle */
    send(hid);
    map(c[3], HID_MEMORY);
    stage = 2;
    volatile u32* memory = (volatile u32*)HID_MEMORY;
    u32 last = memory[0x10 / 4];
    for (;;) {
        /* Pad and touch entries are written in the same HID update (hid.cpp
         * UpdatePadCallback), so the touch entry for a pad entry is as far behind
         * touch.index as the pad entry is behind pad.index. */
        u32 index = memory[0x10 / 4];
        u32 touch_index = memory[0xB8 / 4];
        while (last != index) {
            last = (last + 1) % 8;
            u32 pad = memory[(0x28 + 16 * last) / 4];
            u32 behind = (index + 8 - last) % 8;
            volatile u32* entry = &memory[(0xC8 + 8 * ((touch_index + 8 - behind) % 8)) / 4];
            u32 point = (entry[1] & 0x7f) ? entry[0] : 0;
            ++updates;
            checksum = fold(fold(checksum, pad), point);
            if (pad != buttons || point != touch) {
                if (transitions < 64) {
                    changes[transitions][0] = updates;
                    changes[transitions][1] = pad;
                    changes[transitions][2] = point;
                }
                ++transitions;
                buttons = pad;
                touch = point;
            }
        }
        sleep_ms(1);
    }
}
__attribute__((naked, section(".text.start"), used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
