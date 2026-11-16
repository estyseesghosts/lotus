/* Owned peripheral readback fixture. IPC/SVC ABI: official libctru
 * source/services/{hid,irrst,ptmu,dsp,cam,nfc}.c and source/svc.s; native
 * service/{hid,ir,ptm,dsp,cam,nfc}. Shared memory is mapped for the host to
 * decode; service replies are copied into `results`. */
typedef unsigned int u32;
typedef unsigned short u16;
volatile u32 stage=1,failure=0,failed_stage=0;
volatile u32 results[32];
volatile u16 camera_buffer[160*120] __attribute__((aligned(4096)));
#define HID_MEMORY 0x10000000u
#define IR_MEMORY 0x10002000u
static volatile u32* command(void) {
    u32 tls;__asm__ volatile("mrc p15, 0, %0, c13, c0, 3":"=r"(tls));return (volatile u32*)(tls+0x80);
}
static void panic(u32 code) {
    failure=code;failed_stage=stage;register u32 r0 __asm__("r0")=0;
    __asm__ volatile("svc 0x3c":"+r"(r0)::"memory");for(;;);
}
static u32 request(u32 handle) {
    register u32 r0 __asm__("r0")=handle;
    __asm__ volatile("svc 0x32":"+r"(r0)::"r1","r2","r3","memory");
    if(r0)panic(r0);return command()[1];
}
static void send(u32 handle) {u32 result=request(handle);if(result)panic(result);}
static u32 connect(void) {
    register u32 r0 __asm__("r0")=0;register const char* r1 __asm__("r1")="srv:";
    __asm__ volatile("svc 0x2d":"+r"(r0),"+r"(r1)::"r2","r3","memory");
    if(r0)panic(r0);return (u32)r1;
}
static u32 service(u32 srv,const char* name) {
    volatile u32* c=command();u32 length=0;
    c[0]=0x50100;c[1]=0;c[2]=0;
    while(name[length]){((volatile char*)&c[1])[length]=name[length];++length;}
    c[3]=length;c[4]=0;send(srv);return c[3];
}
static void map(u32 handle,u32 address) {
    register u32 r0 __asm__("r0")=handle,r1 __asm__("r1")=address,
        r2 __asm__("r2")=1,r3 __asm__("r3")=0x10000000;
    __asm__ volatile("svc 0x1f":"+r"(r0),"+r"(r1),"+r"(r2),"+r"(r3)::"memory");
    if(r0)panic(r0);
}
static void sleep_ms(u32 ms) {
    unsigned long long ns=(unsigned long long)ms*1000000ull;
    register u32 r0 __asm__("r0")=(u32)ns,r1 __asm__("r1")=(u32)(ns>>32);
    __asm__ volatile("svc 0x0a":"+r"(r0),"+r"(r1)::"r2","r3","memory");
}
static u32 wait(u32 handle,u32 ms) {
    unsigned long long ns=(unsigned long long)ms*1000000ull;
    register u32 r0 __asm__("r0")=handle,r2 __asm__("r2")=(u32)ns,r3 __asm__("r3")=(u32)(ns>>32);
    __asm__ volatile("svc 0x24":"+r"(r0),"+r"(r2),"+r"(r3)::"r1","memory");
    return r0;
}
__attribute__((noinline)) int main(void) {
    volatile u32* c=command();u32 srv=connect();
    c[0]=0x10002;c[1]=0x20;c[2]=0;send(srv);stage=2;
    /* HID: shared memory, accelerometer, gyroscope, sound volume. */
    u32 hid=service(srv,"hid:USER");
    c[0]=0xa0000;send(hid);map(c[3],HID_MEMORY);
    c[0]=0x110000;send(hid);c[0]=0x130000;send(hid);
    c[0]=0x170000;send(hid);results[0]=c[2]&0xff;stage=3;
    /* IR: ZL/ZR and C-stick shared memory, 8 ms period, processed directions. */
    u32 ir=service(srv,"ir:rst");
    c[0]=0x10000;send(ir);map(c[3],IR_MEMORY);
    c[0]=0x20080;c[1]=8;c[2]=0;send(ir);stage=4;
    /* PTM battery and DSP headphone state. */
    u32 ptm=service(srv,"ptm:u");
    c[0]=0x50000;send(ptm);results[1]=c[2]&0xff;
    c[0]=0x70000;send(ptm);results[2]=c[2]&0xff;
    c[0]=0x80000;send(ptm);results[3]=c[2]&0xff;
    u32 dsp=service(srv,"dsp::DSP");
    c[0]=0x1f0000;send(dsp);results[4]=c[2]&0xff;
    stage=5;
    /* Camera: outer camera 1 (port 1), QQVGA RGB565, one frame. */
    u32 cam=service(srv,"cam:u");
    c[0]=0x1f00c0;c[1]=1;c[2]=2;c[3]=1;send(cam);
    c[0]=0x2500c0;c[1]=1;c[2]=1;c[3]=1;send(cam);
    c[0]=0xb0100;c[1]=1;c[2]=160*2*4;c[3]=160;c[4]=120;send(cam);
    c[0]=0x130040;c[1]=1;send(cam);
    c[0]=0x40040;c[1]=1;send(cam);
    c[0]=0x10040;c[1]=1;send(cam);
    c[0]=0x70102;c[1]=(u32)camera_buffer;c[2]=1;c[3]=sizeof(camera_buffer);c[4]=160*2*4;
    c[5]=0;c[6]=0xffff8001;send(cam);u32 received=c[3];
    results[6]=wait(received,2000);
    c[0]=0x20040;c[1]=1;send(cam);stage=6;
    /* NFC: scan until the host mounts a tag, then read the tag UID. The 3D
     * slider float in the shared page is re-read while waiting. */
    u32 nfc=service(srv,"nfc:u");
    c[0]=0x10040;c[1]=2;send(nfc);
    c[0]=0x50040;c[1]=0;send(nfc);stage=7;
    for(u32 tries=0;;++tries) {
        results[5]=*(volatile u32*)0x1ff81080;
        c[0]=0xd0000;send(nfc);results[7]=c[2]&0xff;
        if(results[7]==3)break;
        if(tries>2000)panic(0xdead0007);
        sleep_ms(5);
    }
    c[0]=0x110000;send(nfc);
    results[8]=c[2];results[9]=c[3];results[10]=c[4];stage=8;
    for(;;)sleep_ms(16);
}
__attribute__((naked,section(".text.start"),used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
