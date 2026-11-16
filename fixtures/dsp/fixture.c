/* Owned DSP semaphore/IRQ fixture; firmware is a separate private user asset. IPC layouts: native
 * core/hle/service/dsp/dsp_dsp.cpp and devkitPro/libctru source/services/dsp.c.
 * Shared voice ABI: native audio_core/hle/shared_memory.h; this is an emulator
 * acceptance fixture, not a claim about arbitrary DSP firmware or hardware.
 */
typedef unsigned int u32;
typedef unsigned short u16;
typedef signed short s16;
volatile u32 stage=1;
volatile u32 failure=0;

static volatile u32* command(void) {
    u32 tls;__asm__ volatile("mrc p15, 0, %0, c13, c0, 3":"=r"(tls));
    return (volatile u32*)(tls+0x80);
}
static void panic(u32 code) {
    failure=code;
    register u32 r0 __asm__("r0")=0;
    __asm__ volatile("svc 0x3c":"+r"(r0)::"memory");
    for(;;);
}
static void send(u32 handle) {
    register u32 r0 __asm__("r0")=handle;
    __asm__ volatile("svc 0x32":"+r"(r0)::"r1","r2","r3","memory");
    if(r0 || command()[1])panic(r0?r0:command()[1]);
}
static u32 connect(void) {
    register u32 r0 __asm__("r0")=0;
    register const char* r1 __asm__("r1")="srv:";
    __asm__ volatile("svc 0x2d":"+r"(r0),"+r"(r1)::"r2","r3","memory");
    if(r0)panic(r0);
    return (u32)r1;
}
static u32 linear(void) {
    register u32 r0 __asm__("r0")=0x10003,r1 __asm__("r1")=0,r2 __asm__("r2")=0,
        r3 __asm__("r3")=4096,r4 __asm__("r4")=3;
    __asm__ volatile("svc 1":"+r"(r0),"+r"(r1),"+r"(r2),"+r"(r3),"+r"(r4)::"memory");
    if(r0)panic(r0);return r1;
}
static u32 irq_event,dsp_semaphore;
static u32 create_event(void) {
    register u32 r0 __asm__("r0")=0,r1 __asm__("r1")=1;
    __asm__ volatile("svc 0x17":"+r"(r0),"+r"(r1)::"r2","r3","memory");
    if(r0)panic(r0);return r1;
}
static void event_action(u32 event,int wait) {
    register u32 r0 __asm__("r0")=event,r2 __asm__("r2")=~0u,r3 __asm__("r3")=~0u;
    if(wait)__asm__ volatile("svc 0x24":"+r"(r0),"+r"(r2),"+r"(r3)::"r1","memory");
    else __asm__ volatile("svc 0x18":"+r"(r0)::"r1","r2","r3","memory");
    if(r0)panic(r0);
}
static void wait_irq(void) {
    event_action(irq_event,1);register u32 r0 __asm__("r0")=irq_event;
    __asm__ volatile("svc 0x19":"+r"(r0)::"r1","r2","r3","memory");if(r0)panic(r0);
}
static void read_pipe(u32 dsp,void* output,u32 length) {
    volatile u32* c=command();c[0]=0x1000c0;c[1]=2;c[2]=0;c[3]=length;
    c[64]=(length<<14)|2;c[65]=(u32)output;send(dsp);
    if(c[2]!=length)panic(0x80000001);
}
static u32 convert(u32 dsp,u32 address) {
    volatile u32* c=command();c[0]=0xc0040;c[1]=address;send(dsp);return c[2];
}
static u32 rotate(u32 value) {return (value<<16)|(value>>16);}
__attribute__((noinline)) int main(void) {
    volatile u32* c=command();u32 srv=connect();stage=2;
    c[0]=0x10002;c[1]=0x20;c[2]=0;send(srv);
    static const char service[8]={'d','s','p',':',':','D','S','P'};
    c[0]=0x50100;for(u32 i=0;i<8;++i)((volatile char*)&c[1])[i]=service[i];
    c[3]=8;c[4]=0;send(srv);u32 dsp=c[3];stage=3;
    irq_event=create_event();
    c[0]=0x150082;c[1]=2;c[2]=2;c[3]=0;c[4]=irq_event;send(dsp);
    c[0]=0x160000;send(dsp);dsp_semaphore=c[3];
    c[0]=0x170040;c[1]=0x2000;send(dsp);
    u32 initialize=0;c[0]=0xd0082;c[1]=2;c[2]=4;c[3]=(4<<14)|(1<<10)|2;c[4]=(u32)&initialize;send(dsp);
    c[0]=0x70040;c[1]=0x4000;send(dsp);wait_irq();
    stage=4;
    u16 count=0,addresses[16];read_pipe(dsp,&count,2);
    if(count!=15)panic(0x80000002);
    read_pipe(dsp,addresses,count*2);
    u32 voice=convert(dsp,addresses[1]),master=convert(dsp,addresses[4]),counter=convert(dsp,addresses[0]);
    const u32 sample_address=linear();volatile s16* samples=(volatile s16*)sample_address;
    for(u32 i=0;i<256;++i)samples[i]=i<128?8192:-8192;
    const u32 physical=sample_address>=0x30000000?sample_address-0x10000000:sample_address+0xc000000;
    for(u32 region=0;region<2;++region) {
        /* The second shared bank is 0x20000 ARM bytes from the first. */
        volatile u32* v=(volatile u32*)(voice+region*0x20000);
        for(u32 i=0;i<192/4;++i)v[i]=0;
        v[1]=v[2]=0x3f800000; /* gain, left and right */
        v[13]=0x3f800000; /* resampling ratio 1.0 */
        ((volatile unsigned char*)v)[56]=2; /* no interpolation */
        ((volatile unsigned char*)v)[160]=1;
        v[43]=rotate(physical);v[44]=rotate(256);
        ((volatile u16*)v)[90]=5; /* mono PCM16 */
        ((volatile u16*)v)[94]=2; /* looping */
        ((volatile u16*)v)[95]=1;
        v[0]=0xffffffff;
        volatile u32* m=(volatile u32*)(master+region*0x20000);
        for(u32 i=0;i<196/4;++i)m[i]=0;
        m[1]=0x3f800000;((volatile u16*)m)[8]=1;((volatile u16*)m)[11]=1;m[0]=0xffffffff;
    }
    c[0]=0x70040;c[1]=0x4000;send(dsp);
    *(volatile u16*)counter=4;stage=5;
    event_action(dsp_semaphore,0);
    static const char ready[]="lotus-audio:ready\n";
    register const char* r0 __asm__("r0")=ready;register u32 r1 __asm__("r1")=sizeof(ready)-1;
    __asm__ volatile("svc 0x3d":"+r"(r0),"+r"(r1)::"r2","r3","memory");
    u32 frame=5;
    for(;;) {
        wait_irq();
        u32 received=*(volatile u16*)(counter+((~frame&1)*0x20000));
        if(received){frame=(received+1)&0xffff;if(!frame)frame=2;}
        *(volatile u16*)(counter+((frame&1)*0x20000))=(u16)frame;
        ++frame;event_action(dsp_semaphore,0);
    }
}
__attribute__((naked,section(".text.start"),used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
