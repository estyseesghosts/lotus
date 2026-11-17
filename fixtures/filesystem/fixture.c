/* Owned SDMC write fixture. The host writes `path`/`payload` and bumps
 * `request`; the guest opens the file with FSUSER_OpenFileDirectly on the SDMC
 * archive, writes the payload and records the result codes. IPC layout: official
 * libctru source/services/fs.c (FSUSER_Initialize, OpenFileDirectly,
 * FSFILE_Write, FSFILE_Close); native service/fs/fs_user.cpp. */
typedef unsigned int u32;
volatile u32 stage=1,failure=0,failed_stage=0;
volatile u32 request=0,done=0,open_result=0,write_result=0,written=0;
volatile u32 payload_size=0;
/* Churn (#28): while `churn` is set the guest keeps /lotus-churn.bin open and rewrites
 * all 256 words with `generation` in one FSFILE_Write without FS_WRITE_FLUSH, so the bytes
 * can sit in the worker's stdio buffer. `generation_done` follows each completed write. */
volatile u32 churn=0,generation=0,generation_done=0,churn_result=0;
static u32 churn_words[256];
static u32 churn_file=0;
volatile char path[256]="/lotus-fs.txt";
volatile char payload[256];
static const char empty[1]="";
static volatile u32* command(void) {
    u32 tls;__asm__ volatile("mrc p15, 0, %0, c13, c0, 3":"=r"(tls));return (volatile u32*)(tls+0x80);
}
static void panic(u32 code) {
    failure=code;failed_stage=stage;register u32 r0 __asm__("r0")=0;
    __asm__ volatile("svc 0x3c":"+r"(r0)::"memory");for(;;);
}
static u32 request_ipc(u32 handle) {
    register u32 r0 __asm__("r0")=handle;
    __asm__ volatile("svc 0x32":"+r"(r0)::"r1","r2","r3","memory");
    if(r0)panic(r0);return command()[1];
}
static void send(u32 handle) {u32 result=request_ipc(handle);if(result)panic(result);}
static void close_handle(u32 handle) {
    register u32 r0 __asm__("r0")=handle;
    __asm__ volatile("svc 0x23":"+r"(r0)::"r1","r2","r3","memory");
}
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
static void sleep_ms(u32 ms) {
    unsigned long long ns=(unsigned long long)ms*1000000ull;
    register u32 r0 __asm__("r0")=(u32)ns,r1 __asm__("r1")=(u32)(ns>>32);
    __asm__ volatile("svc 0x0a":"+r"(r0),"+r"(r1)::"r2","r3","memory");
}
static void write_file(u32 fs) {
    volatile u32* c=command();u32 length=0;
    while(length<255&&path[length])++length;
    c[0]=0x8030204;c[1]=0;c[2]=9;              /* ARCHIVE_SDMC */
    c[3]=1;c[4]=1;                              /* PATH_EMPTY, size 1 */
    c[5]=3;c[6]=length+1;                       /* PATH_ASCII */
    c[7]=2|4;c[8]=0;                            /* FS_OPEN_WRITE|FS_OPEN_CREATE */
    c[9]=(1<<14)|(2<<10)|2;c[10]=(u32)empty;
    c[11]=((length+1)<<14)|2;c[12]=(u32)path;
    open_result=request_ipc(fs);written=0;write_result=~0u;
    if(open_result)return;
    u32 file=c[3],size=payload_size>256?256:payload_size;
    c[0]=0x8030102;c[1]=0;c[2]=0;c[3]=size;c[4]=1;  /* FS_WRITE_FLUSH */
    c[5]=(size<<4)|0x8|2;c[6]=(u32)payload;         /* IPC_BUFFER_R */
    write_result=request_ipc(file);written=c[2];
    c[0]=0x8080000;request_ipc(file);close_handle(file);
}
static void churn_once(u32 fs) {
    volatile u32* c=command();
    static const char name[]="/lotus-churn.bin";
    if(!churn_file) {
        c[0]=0x8030204;c[1]=0;c[2]=9;c[3]=1;c[4]=1;c[5]=3;c[6]=sizeof(name);
        c[7]=2|4;c[8]=0;
        c[9]=(1<<14)|(2<<10)|2;c[10]=(u32)empty;
        c[11]=(sizeof(name)<<14)|2;c[12]=(u32)name;
        churn_result=request_ipc(fs);
        if(churn_result)return;
        churn_file=c[3];
    }
    u32 next=generation+1;
    for(u32 i=0;i<256;++i)churn_words[i]=next;
    generation=next;
    c[0]=0x8030102;c[1]=0;c[2]=0;c[3]=sizeof(churn_words);c[4]=0;  /* no FS_WRITE_FLUSH */
    c[5]=(sizeof(churn_words)<<4)|0x8|2;c[6]=(u32)churn_words;
    churn_result=request_ipc(churn_file);
    if(!churn_result)generation_done=next;
}
__attribute__((noinline)) int main(void) {
    volatile u32* c=command();u32 srv=connect();
    c[0]=0x10002;c[1]=0x20;c[2]=0;send(srv);
    u32 fs=service(srv,"fs:USER");
    c[0]=0x8010002;c[1]=0x20;c[2]=0;send(fs);stage=2;
    for(;;) {
        if(request!=done){write_file(fs);done=request;}
        if(churn)churn_once(fs);
        sleep_ms(4);
    }
}
__attribute__((naked,section(".text.start"),used)) void _start(void) {
    __asm__ volatile("bl main\n1: b 1b");
}
