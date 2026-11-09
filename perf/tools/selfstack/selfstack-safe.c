#include <signal.h>
#include <unistd.h>
#include <pthread.h>
#include <mach/mach.h>
#include <fcntl.h>
#include <stdlib.h>
#include <stdint.h>
#include <sys/ucontext.h>
#include <stdio.h>
#include <mach-o/dyld.h>
#include <stdatomic.h>
static int out=-1;
static _Atomic uintptr_t stack_low,stack_high;
static _Atomic int active=0;
static _Atomic int completed=0;
static char *hex(char *p,uintptr_t v){*p++='0';*p++='x';for(int i=15;i>=0;i--)*p++="0123456789abcdef"[(v>>(i*4))&15];return p;}
static void capture(int sig,siginfo_t *info,void *ctx){(void)sig;(void)info;ucontext_t *uc=ctx;uintptr_t sp=uc->uc_mcontext->__ss.__sp;uintptr_t low=atomic_load(&stack_low),high=atomic_load(&stack_high);if(sp<low || sp>=high)return;int expected=1;if(!atomic_compare_exchange_strong(&active,&expected,2))return;low=atomic_load(&stack_low);high=atomic_load(&stack_high);if(sp<low || sp>=high){expected=2;atomic_compare_exchange_strong(&active,&expected,1);return;}char b[4096];char *p=b;*p++='S';*p++='T';*p++='A';*p++='C';*p++='K';*p++=' ';p=hex(p,uc->uc_mcontext->__ss.__pc);*p++=' ';p=hex(p,uc->uc_mcontext->__ss.__lr);uintptr_t *fp=(uintptr_t *)uc->uc_mcontext->__ss.__fp;for(unsigned k=0;k<96;k++){if((uintptr_t)fp<low || high<2*sizeof(uintptr_t) || (uintptr_t)fp>high-2*sizeof(uintptr_t) || ((uintptr_t)fp&15))break;uintptr_t next=fp[0],ret=fp[1];if(!ret)break;*p++=' ';p=hex(p,ret);if(next<=(uintptr_t)fp)break;fp=(uintptr_t *)next;}*p++='\n';write(out,b,p-b);atomic_store_explicit(&completed,1,memory_order_release);}
static int parameter(const char *name,unsigned fallback,unsigned min,unsigned max,unsigned *value){const char *s=getenv(name);if(!s){*value=fallback;return 1;}if(!*s)return 0;unsigned n=0;for(;*s;s++){if(*s<'0'||*s>'9')return 0;unsigned digit=(unsigned)(*s-'0');if(n>(max-digit)/10)return 0;n=n*10+digit;if(n>max)return 0;}if(n<min)return 0;*value=n;return 1;}
static void *worker(void *arg){(void)arg;unsigned delay,rounds,interval;if(!parameter("SELFSTACK_DELAY_MS",100,0,60000,&delay)||!parameter("SELFSTACK_ROUNDS",20,1,100,&rounds)||!parameter("SELFSTACK_INTERVAL_MS",10,0,60000,&interval)){dprintf(out,"STOP invalid sampler parameter\n");return 0;}usleep(delay*1000);for(unsigned k=0;k<rounds;k++){thread_act_array_t ts;mach_msg_type_number_t n;if(task_threads(mach_task_self(),&ts,&n)!=KERN_SUCCESS)break;for(unsigned i=0;i<n;i++){pthread_t t=pthread_from_mach_thread_np(ts[i]);if(t&&!pthread_equal(t,pthread_self())){stack_high=(uintptr_t)pthread_get_stackaddr_np(t);stack_low=stack_high-pthread_get_stacksize_np(t);dprintf(out,"ROUND %u THREAD %u BOUNDS %p %p\n",k,ts[i],(void*)stack_low,(void*)stack_high);atomic_store_explicit(&completed,0,memory_order_relaxed);atomic_store(&active,1);if(pthread_kill(t,SIGUSR2)==0){unsigned wait=0;while(!atomic_load_explicit(&completed,memory_order_acquire)&&wait++<100)usleep(1000);if(!atomic_load_explicit(&completed,memory_order_acquire)){atomic_store(&active,0);dprintf(out,"STOP blocked signal; abandoning sampler\n");return 0;}}atomic_store(&active,0);}mach_port_deallocate(mach_task_self(),ts[i]);}vm_deallocate(mach_task_self(),(vm_address_t)ts,n*sizeof(thread_t));usleep(interval*1000);}return 0;}
__attribute__((constructor)) static void init(void){const char *dir=getenv("PDRS_PROFILE_DIR");if(!dir)return;struct sigaction old;if(sigaction(SIGUSR2,0,&old)||old.sa_handler!=SIG_DFL)return;char path[4096];snprintf(path,sizeof(path),"%s/safestack-%d.txt",dir,getpid());out=open(path,O_CREAT|O_WRONLY|O_APPEND,0600);if(out<0)return;dprintf(out,"PID %d SAFE raw FP stack; no symbolization in handler\n",getpid());for(uint32_t i=0;i<_dyld_image_count();i++)dprintf(out,"IMAGE %p SLIDE %p %s\n",_dyld_get_image_header(i),(void*)_dyld_get_image_vmaddr_slide(i),_dyld_get_image_name(i));struct sigaction sa={0};sa.sa_sigaction=capture;sa.sa_flags=SA_SIGINFO;sigaction(SIGUSR2,&sa,0);pthread_t t;if(!pthread_create(&t,0,worker,0))pthread_detach(t);}
