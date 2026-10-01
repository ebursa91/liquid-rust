#define _GNU_SOURCE
#include <linux/perf_event.h>
#include <sys/syscall.h>
#include <sys/mman.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <time.h>
static volatile sig_atomic_t stopped;
static void stop(int sig) { (void)sig; stopped=1; }
static void copy_ring(unsigned char *out, const unsigned char *ring, uint64_t pos, size_t size, size_t capacity) {
 size_t offset=(size_t)(pos&(capacity-1)); size_t first=capacity-offset; if(first>size)first=size;
 memcpy(out,ring+offset,first); if(first<size)memcpy(out+first,ring,size-first);
}
int main(int argc,char **argv) {
 if(argc!=3){fprintf(stderr,"usage: sampler OWN_RENDERER_PID OUTPUT\n");return 2;}
 char *end; long parsed=strtol(argv[1],&end,10); if(*end||parsed<=1)return 2; pid_t pid=(pid_t)parsed;
 char status[64]; snprintf(status,sizeof(status),"/proc/%ld/status",parsed);
 FILE *s=fopen(status,"r"); if(!s)return 2; char line[1024]; unsigned uid=~0U;
 while(fgets(line,sizeof(line),s)) { if(sscanf(line,"Uid:\t%u",&uid)==1)break; }
 fclose(s);
 if(uid!=getuid()){fprintf(stderr,"target is not owned by current user\n");return 2;}
 struct perf_event_attr event={0}; event.size=sizeof(event); event.type=PERF_TYPE_SOFTWARE;
 event.config=PERF_COUNT_SW_CPU_CLOCK; event.exclude_kernel=1; event.exclude_hv=1;
 event.disabled=1; event.freq=1; event.sample_freq=1000;
 event.sample_type=PERF_SAMPLE_IP|PERF_SAMPLE_TIME|PERF_SAMPLE_CALLCHAIN;
 event.wakeup_events=1;
 int fd=(int)syscall(SYS_perf_event_open,&event,pid,-1,-1,PERF_FLAG_FD_CLOEXEC);
 if(fd<0){fprintf(stderr,"perf_event_open: %s\n",strerror(errno));return 3;}
 size_t page=(size_t)sysconf(_SC_PAGESIZE), capacity=page*256;
 void *mapping=mmap(NULL,page+capacity,PROT_READ|PROT_WRITE,MAP_SHARED,fd,0);
 if(mapping==MAP_FAILED){perror("mmap");close(fd);return 4;}
 struct perf_event_mmap_page *meta=mapping;
 if(meta->data_size!=capacity||meta->data_offset!=page){fprintf(stderr,"unexpected ring layout\n");return 4;}
 unsigned char *ring=(unsigned char*)mapping+meta->data_offset;
 FILE *output=fopen(argv[2],"w"); if(!output){perror("output");return 4;}
 signal(SIGTERM,stop);signal(SIGINT,stop);
 ioctl(fd,PERF_EVENT_IOC_RESET,0); if(ioctl(fd,PERF_EVENT_IOC_ENABLE,0)){perror("enable");return 5;}
 puts("ready");fflush(stdout); uint64_t tail=0,samples=0,lost=0,unknown=0;
 do {
  if(stopped)ioctl(fd,PERF_EVENT_IOC_DISABLE,0);
  uint64_t head=__atomic_load_n(&meta->data_head,__ATOMIC_ACQUIRE);
  while(tail<head){
   struct perf_event_header header; copy_ring((unsigned char*)&header,ring,tail,sizeof(header),capacity);
   if(header.size<sizeof(header)||header.size>capacity||tail+header.size>head){fprintf(stderr,"invalid record\n");return 6;}
   unsigned char *record=malloc(header.size); if(!record)return 6; copy_ring(record,ring,tail,header.size,capacity);
   if(header.type==PERF_RECORD_SAMPLE){
    size_t count=(header.size-sizeof(header))/8; uint64_t *fields=(uint64_t*)(record+sizeof(header));
    if(count<3||fields[2]>count-3){fprintf(stderr,"invalid sample\n");return 6;}
    fprintf(output,"%llu %llx",(unsigned long long)fields[1],(unsigned long long)fields[0]);
    for(size_t i=0;i<fields[2];i++) { fprintf(output," %llx",(unsigned long long)fields[3+i]); }
    fputc('\n',output); samples++;
   } else if(header.type==PERF_RECORD_LOST){uint64_t *fields=(uint64_t*)(record+sizeof(header));if(header.size>=sizeof(header)+16)lost+=fields[1];}
   else unknown++;
   free(record); tail+=header.size;
  }
  __atomic_store_n(&meta->data_tail,tail,__ATOMIC_RELEASE);
  if(!stopped){struct timespec pause={0,10000000};nanosleep(&pause,NULL);}
 } while(!stopped||tail<__atomic_load_n(&meta->data_head,__ATOMIC_ACQUIRE));
 ioctl(fd,PERF_EVENT_IOC_DISABLE,0); fclose(output);munmap(mapping,page+capacity);close(fd);
 fprintf(stderr,"{\"samples\":%llu,\"lost\":%llu,\"other_records\":%llu,\"requested_hz\":1000,\"exclude_kernel\":true}\n",(unsigned long long)samples,(unsigned long long)lost,(unsigned long long)unknown);
 return 0;
}
