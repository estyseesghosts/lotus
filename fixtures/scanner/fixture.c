/* Owned ARM fixture. Two scratch pages precede a 1 MiB zero-filled scan region. */
volatile unsigned char scan_area[8192 + 1048576] __attribute__((aligned(4096))) = {1};
const unsigned int scan_marker = 0x5343414e;

__attribute__((section(".text.start"), used)) void _start(void) {
    for (;;) {
        __asm__ volatile("nop");
    }
}
