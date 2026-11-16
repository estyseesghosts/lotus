/* Owned mic:u shared-buffer readback fixture. IPC/SVC ABI: official libctru
 * source/services/mic.c, source/svc.s; native service/mic/mic_u.cpp. */
typedef unsigned int u32;
volatile u32 stage=1,failure=0;
volatile unsigned char mic_buffer[4096] __attribute__((aligned(4096)));
static volatile u32* command(void) {
    u32 tls;__asm__ volatile("mrc p15, 0, %0, c13, c0, 3":"=r"(tls));return (volatile u32*)(tls+0x80);
}
static void panic(u32 code) {
    failure=code;register u32 r0 __asm__("r0")=0;
    __asm__ volatile("svc 0x3c":"+r"(r0)::"memory");for(;;);
}
static void send(u32 handle) {
    register u32 r0 __asm__("r0")=handle;
    __asm__ volatile("svc 0x32":"+r"(r0)::"r1","r2","r3","memory");
    if(r0||command()[1])panic(r0?r0:command()[1]);
}
static u32 connect(void) {
    register u32 r0 __asm__("r0")=0;register const char* r1 __asm__("r1")="srv:";
    __asm__ volatile("svc 0x2d":"+r"(r0),"+r"(r1)::"r2","r3","memory");
    if(r0)panic(r0);return (u32)r1;
}
__attribute__((noinline)) int main(void) {
    volatile u32* c=command();u32 srv=connect();stage=2;
    c[0]=0x10002;c[1]=0x20;c[2]=0;send(srv);
    static const char service[8]={'m','i','c',':','u',0,0,0};
    c[0]=0x50100;for(u32 i=0;i<8;++i)((volatile char*)&c[1])[i]=service[i];
    c[3]=5;c[4]=0;send(srv);u32 mic=c[3];stage=3;
    register u32 r0 __asm__("r0")=3,r1 __asm__("r1")=(u32)mic_buffer,
        r2 __asm__("r2")=4096,r3 __asm__("r3")=3;
    __asm__ volatile("svc 0x1e":"+r"(r0),"+r"(r1),"+r"(r2),"+r"(r3)::"memory");
    if(r0)panic(r0);
    c[0]=0x10042;c[1]=4096;c[2]=0;c[3]=r1;send(mic);
    c[0]=0xa0040;c[1]=1;send(mic);stage=4;
    c[0]=0x30140;c[1]=3;c[2]=0;c[3]=0;c[4]=4092;c[5]=1;send(mic);stage=5;
    for(;;)__asm__ volatile("nop");
}
__attribute__((naked,section(".text.start"),used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
