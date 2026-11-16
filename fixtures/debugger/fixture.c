/* Freestanding ARM11 fixture: no firmware, commercial content or host libraries. */
typedef unsigned int u32;
typedef unsigned long long u64;
volatile u32 mode = 0;
volatile u32 watched = 0x11223344;
volatile u32 counter = 1;
volatile u32 canary[2] = {0xcafebabe, 0xdeadbeef};
volatile u32 mutex_a,mutex_b,thread_phase;
volatile u32 raced_counter,race_expected=2,race_ready;
static unsigned char child_stack[4096] __attribute__((aligned(16)));

static u32 create_mutex(u32 locked) {
    register u32 r0 __asm__("r0")=0,r1 __asm__("r1")=locked;
    __asm__ volatile("svc 0x13":"+r"(r0),"+r"(r1)::"r2","r3","memory");
    if(r0)for(;;);return r1;
}
static void wait_mutex(u32 handle) {
    register u32 r0 __asm__("r0")=handle,r2 __asm__("r2")=~0u,r3 __asm__("r3")=~0u;
    __asm__ volatile("svc 0x24":"+r"(r0),"+r"(r2),"+r"(r3)::"r1","memory");
}
static void create_thread(void (*entry)(void),u32 core) {
    register u32 r0 __asm__("r0")=0x20,r1 __asm__("r1")=(u32)entry,r2 __asm__("r2")=0,
        r3 __asm__("r3")=(u32)(child_stack+sizeof(child_stack)),r4 __asm__("r4")=core;
    __asm__ volatile("svc 8":"+r"(r0),"+r"(r1),"+r"(r2),"+r"(r3),"+r"(r4)::"memory");
    if(r0)for(;;);
}
__attribute__((noinline)) void deadlock_child(void) {
    wait_mutex(mutex_b);thread_phase=1;wait_mutex(mutex_a);for(;;);
}
__attribute__((noinline)) void deadlock_main(void) {
    mutex_a=create_mutex(1);mutex_b=create_mutex(0);create_thread(deadlock_child,0);
    while(!thread_phase);wait_mutex(mutex_b);for(;;);
}
__attribute__((noinline)) void race_child(void) {
    u32 old=raced_counter;thread_phase=1;while(thread_phase!=2);
    raced_counter=old+1;thread_phase=3;for(;;);
}
__attribute__((noinline)) void new3ds_race(void) {
    /* svcGetSystemInfo(0x10001) signals New3DS support in its result code.
       Source: core/hle/kernel/svc.cpp, SystemInfoType::NEW_3DS_INFO. */
    register u32 r0 __asm__("r0")=0,r1 __asm__("r1")=0x10001,r2 __asm__("r2")=0;
    __asm__ volatile("svc 0x2a":"+r"(r0),"+r"(r1),"+r"(r2)::"r3","memory");
    if(!r0) {
        create_thread(race_child,2);while(thread_phase!=1);
        u32 old=raced_counter;thread_phase=2;raced_counter=old+1;
        while(thread_phase!=3);
    } else raced_counter=2;
    race_ready=1;for(;;);
}
struct HioRequest {
    char magic[4]; u32 version; char function[17]; char format[9];
    u64 params[8]; u32 lengths[8]; long long result; int error; char interrupted;
};
_Static_assert(sizeof(struct HioRequest) == 152, "libctru HIO v1 layout");
static struct HioRequest hio = {{'G','D','B',0},1,"write","ipi"};
static const char hio_text[] = "lotus-hio:hello\n";

__attribute__((noinline)) void hio_write(void) {
    hio.params[0] = 1;
    hio.params[1] = (u32)hio_text;
    hio.params[2] = sizeof(hio_text)-1;
    register struct HioRequest* r0 __asm__("r0") = &hio;
    register u32 r1 __asm__("r1") = 0;
    __asm__ volatile("svc 0x3d" : "+r"(r0), "+r"(r1) :: "r2", "r3", "memory");
}

__attribute__((noinline)) static void output(const char* text, u32 length) {
    register const char* r0 __asm__("r0") = text;
    register u32 r1 __asm__("r1") = length;
    __asm__ volatile("svc 0x3d" : "+r"(r0), "+r"(r1) :: "r2", "r3", "memory");
}

volatile u32 thumb_counter;
/* Thumb-state code for ARM/Thumb breakpoint and step checks (interworking via BLX). */
__attribute__((noinline, target("thumb"))) void thumb_tick(void) {
    thumb_counter = thumb_counter * 3 + 1;
}

__attribute__((noinline)) void tick(void) {
    __asm__ volatile(".global lotus_tick_body\nlotus_tick_body:" ::: "memory");
    watched = ++counter;
    thumb_tick();
    if (mode == 1) {
        u32 value;
        __asm__ volatile("ldr %0, [%1]" : "=r"(value) : "r"(0) : "memory");
        watched = value;
    }
    if (mode == 2) canary[1] = 0xbad;
    if (mode == 3) {
        register u32 r0 __asm__("r0") = 0;
        __asm__ volatile("svc 0x3c" : "+r"(r0) :: "r1", "r2", "r3", "memory");
    }
    if (mode == 4) __asm__ volatile(".word 0xe7f000f0");
    if (mode == 5) { hio_write(); mode = 3; }
    if (mode == 6) deadlock_main();
    if (mode == 7) new3ds_race();
}

__attribute__((noinline)) int main(void) {
    static const char ready[] = "lotus-fixture:ready\n";
    output(ready, sizeof(ready)-1);
    for (;;) tick();
}

__attribute__((naked, section(".text.start"), used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
