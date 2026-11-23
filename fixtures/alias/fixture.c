/* Owned ARM fixture for Luma bit-31 physical aliases (lotus#155). Each case stores its number
 * in `step` before the access and its observed value in `values[step]` after it, so a worker
 * that aborts leaves the case it died in. `done` is DONE once every case ran. The host test
 * (tests/test_luma_alias.py) reads these with read_memory and matches rejections in the log. */
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;

#define ALIAS(pa) ((pa) | 0x80000000u)
#define DONE 0x454e4f44u
#define FILL_PA 0x18100000u /* VRAM, mapped for 3DSX at 0x1F000000 (loader/3dsx.cpp) */
#define FILL_VA 0x1F100000u

__attribute__((used)) const u32 alias_marker = 0x53414c41u;
volatile u32 step;
volatile u32 done;
volatile u32 fcram_pa;
volatile u32 values[32];
/* Page aligned, so the marker starts a physical page the FCRAM scan can find. */
volatile u32 fcram_page[1024] __attribute__((aligned(4096))) = {0x414d554c, 0x53414c49, 0x35353123};

static u32 read32(u32 address) { return *(volatile u32*)address; }
static void write32(u32 address, u32 value) { *(volatile u32*)address = value; }

static void run(u32 number, u32 value) {
    values[number] = value;
}

__attribute__((section(".text.start"), used)) void _start(void) {
    /* 1: find this page in FCRAM through the alias (O3DS FCRAM is 128 MiB). */
    step = 1;
    for (u32 pa = 0x20000000u; pa < 0x28000000u; pa += 0x1000u) {
        if (read32(ALIAS(pa)) == fcram_page[0] && read32(ALIAS(pa + 4)) == fcram_page[1] &&
            read32(ALIAS(pa + 8)) == fcram_page[2]) {
            fcram_pa = pa;
            break;
        }
    }
    run(1, fcram_pa);
    if (fcram_pa) {
        fcram_page[3] = 0x12345678u;
        step = 2;
        run(2, read32(ALIAS(fcram_pa + 12)));
        step = 3;
        write32(ALIAS(fcram_pa + 16), 0xCAFEF00Du);
        run(3, fcram_page[4]);
        step = 4;
        run(4, *(volatile u8*)ALIAS(fcram_pa));
        step = 5;
        run(5, *(volatile u16*)ALIAS(fcram_pa + 2));
    }
    /* FCRAM ends: 0x28000000 on O3DS, 0x30000000 on N3DS. */
    step = 6;
    run(6, read32(ALIAS(0x27FFFFFCu)));
    step = 7;
    run(7, read32(ALIAS(0x28000000u)));
    step = 8;
    run(8, read32(ALIAS(0x2FFFFFFCu)));
    /* The reported abort: VRAM end + 0x483CA reached GPU::WriteReg(0x271483CA). */
    step = 9;
    write32(ALIAS(0x186483CAu), 0x5A5A5A5Au);
    run(9, 1);
    step = 10;
    run(10, read32(ALIAS(0x186483CAu)));
    /* I/O area 0x10100000..0x10500000, word accesses only. */
    step = 11;
    run(11, read32(ALIAS(0x100FFFFCu)));
    step = 12;
    run(12, read32(ALIAS(0x10100000u)));
    step = 13;
    run(13, read32(ALIAS(0x104FFFFCu)));
    step = 14;
    run(14, read32(ALIAS(0x10500000u)));
    step = 15;
    run(15, read32(ALIAS(0x104FFFFEu)));
    step = 16;
    run(16, *(volatile u16*)ALIAS(0x10400000u));
    step = 17;
    run(17, read32(ALIAS(0x10400002u)));
    /* Emulated registers: LCD page and both GPU pages keep plain register storage. */
    step = 18;
    write32(ALIAS(0x10202000u), 0x01020304u);
    run(18, read32(ALIAS(0x10202000u)));
    step = 19;
    write32(ALIAS(0x10400004u), 0x11111111u);
    run(19, read32(ALIAS(0x10400004u)));
    step = 20;
    write32(ALIAS(0x10401000u), 0x22222222u);
    run(20, read32(ALIAS(0x10401000u)));
    /* 21-22: memory fill 0 triggered through the alias fills 64 bytes of VRAM. */
    step = 21;
    write32(ALIAS(0x10400010u), FILL_PA >> 3);
    write32(ALIAS(0x10400014u), (FILL_PA + 64) >> 3);
    write32(ALIAS(0x10400018u), 0xA5A5A5A5u);
    write32(ALIAS(0x1040001Cu), (1u << 9) | 1u);
    run(21, read32(ALIAS(0x1040001Cu)));
    step = 22;
    run(22, read32(FILL_VA) ^ read32(FILL_VA + 60));
    values[23] = read32(FILL_VA);
    done = DONE;
    for (;;) {
        __asm__ volatile("nop");
    }
}
