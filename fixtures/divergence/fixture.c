/* Owned CPU-backend probe. Each probe stores a result; main then parks the
 * results in r4-r10 inside a branch-to-self loop so matrix checkpoints compare
 * them through registers. Probes are chosen to expose emulator CPU-backend
 * differences (JIT vs interpreter); none is a hardware claim.
 *   0 self-modifying code in RW data, re-executed without an I-cache flush
 *   1 guest ticks (svcGetSystemTick, svc 0x28) across a fixed loop
 *   2 VFP vsqrt(-1.0f) result bits
 *   3 VFP 0.0f * inf result bits
 *   4 VFP denormal 1e-40f * 1.0f result bits
 *   5 unaligned 32-bit load at offset 1 */
typedef unsigned int u32;
volatile u32 results[8];
volatile u32 stage=2;
static u32 code[4] __attribute__((aligned(16)))={0xe3a00001,0xe12fff1e};
static const unsigned char bytes[8]={1,2,3,4,5,6,7,8};
static unsigned long long ticks(void) {
    register u32 r0 __asm__("r0"),r1 __asm__("r1");
    __asm__ volatile("svc 0x28":"=r"(r0),"=r"(r1)::"r2","r3","memory");
    return ((unsigned long long)r1<<32)|r0;
}
static float fbits(u32 v){union{u32 u;float f;}x={v};return x.f;}
static u32 bits(float f){union{float f;u32 u;}x={f};return x.u;}
__attribute__((noinline)) static u32 call(void){return ((u32(*)(void))code)();}
__attribute__((noinline)) int main(void) {
    code[0]=0xe3a00001;code[1]=0xe12fff1e;           /* mov r0,#1; bx lr */
    u32 first=call();
    code[0]=0xe3a00002;                               /* mov r0,#2, no flush */
    results[0]=(first<<8)|call();
    unsigned long long start=ticks();
    for(volatile u32 i=0;i<10000;++i);
    results[1]=(u32)(ticks()-start);
    float minus_one=fbits(0xbf800000),zero=fbits(0),inf=fbits(0x7f800000),tiny=fbits(0x000116c2);
    float out;
    __asm__ volatile("vsqrt.f32 %0,%1":"=t"(out):"t"(minus_one));results[2]=bits(out);
    __asm__ volatile("vmul.f32 %0,%1,%2":"=t"(out):"t"(zero),"t"(inf));results[3]=bits(out);
    __asm__ volatile("vmul.f32 %0,%1,%2":"=t"(out):"t"(tiny),"t"(fbits(0x3f800000)));results[4]=bits(out);
    u32 unaligned;__asm__ volatile("ldr %0,[%1]":"=r"(unaligned):"r"(bytes+1));results[5]=unaligned;
    stage=3;  /* probes done */
    register u32 r4 __asm__("r4")=results[0],r5 __asm__("r5")=results[1],r6 __asm__("r6")=results[2],
        r7 __asm__("r7")=results[3],r8 __asm__("r8")=results[4],r9 __asm__("r9")=results[5];
    __asm__ volatile("1: b 1b"::"r"(r4),"r"(r5),"r"(r6),"r"(r7),"r"(r8),"r"(r9));
    return 0;
}
__attribute__((naked,section(".text.start"),used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
