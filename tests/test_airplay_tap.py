"""Real dynamic-loader test: observer must preserve ALSA samples, errors and short writes."""
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

@unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('cc'), 'Linux dynamic loader')
class AirplayTap(unittest.TestCase):
    def test_pcm_copy_short_write_errors_and_missing_listener(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            (work/'mock.c').write_text(r'''
#include <errno.h>
#include <stdlib.h>
#include <stdint.h>
int snd_pcm_hw_params_malloc(void **p) { *p=malloc(1); return 0; }
void snd_pcm_hw_params_free(void *p) { free(p); }
int snd_pcm_hw_params_current(void *p, void *q) { return 0; }
int snd_pcm_hw_params_get_rate(const void *p,unsigned *r,int *d) { *r=48000; return 0; }
int snd_pcm_hw_params_get_channels(const void *p,unsigned *c) { *c=2; return 0; }
int snd_pcm_hw_params_get_format(const void *p,int *f) { *f=2; return 0; }
long snd_pcm_writei(void *p,const void *b,unsigned long n) {
 const int16_t *s=b;
 for(unsigned i=0;i<n*2;i++) if(s[i]!=(int16_t)i) abort();
 errno=123;
 return p==(void*)2 ? -32 : (long)n-1;
}
long snd_pcm_mmap_writei(void *p,const void *b,unsigned long n) { return snd_pcm_writei(p,b,n); }
''')
            (work/'main.c').write_text(r'''
#include <assert.h>
#include <errno.h>
#include <stdint.h>
long snd_pcm_writei(void *,const void *,unsigned long);
int main(void) {
 int16_t data[2400]; for(int i=0;i<2400;i++)data[i]=i;
 assert(snd_pcm_writei((void*)1,data,1200)==1199); assert(errno==123);
 assert(snd_pcm_writei((void*)2,data,1200)==-32); assert(errno==123);
 for(int i=0;i<2400;i++)assert(data[i]==i);
 return 0;
}
''')
            def cc(*args):
                subprocess.run(['cc', *args], cwd=work, check=True, capture_output=True)
            cc('-shared','-fPIC','mock.c','-o','mock.so')
            cc('main.c','-L.','-l:mock.so','-Wl,-rpath,'+str(work),'-o','shairport-sync')
            cc('-shared','-fPIC',str(ROOT/'controls/airplay_tap.c'),'-ldl','-pthread','-o','tap.so')
            endpoint=str(work/'pcm.sock')
            env=os.environ | {'LD_PRELOAD':str(work/'tap.so'), 'SPOTIYA_SPECTRUM_SOCKET':endpoint}
            with socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM) as listener:
                listener.bind(endpoint); listener.settimeout(0.5)
                subprocess.run([str(work/'shairport-sync')],env=env,check=True)
                packets=[listener.recv(8192),listener.recv(8192)]
                for packet in packets:
                    self.assertEqual(packet[:8],b'SPV\x01'+struct.pack('<I',48000))
                pcm=b''.join(p[8:] for p in packets)
                self.assertEqual(pcm,struct.pack('<2398h',*range(2398)))
                with self.assertRaises(socket.timeout): listener.recv(8192)
            Path(endpoint).unlink()
            subprocess.run([str(work/'shairport-sync')],env=env,check=True,timeout=2)

    def test_ducking_without_spectrum_restores_and_preserves_short_write_progress(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            (work/'mock.c').write_text(r'''
#include <assert.h>
#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <math.h>
#include <stdio.h>
int phase, failure, short_write;
static unsigned accepted;
int snd_pcm_hw_params_malloc(void **p) { *p=malloc(1); return 0; }
void snd_pcm_hw_params_free(void *p) { free(p); }
int snd_pcm_hw_params_current(void *p, void *q) { return 0; }
int snd_pcm_hw_params_get_rate(const void *p,unsigned *r,int *d) { *r=atoi(getenv("RATE")); return 0; }
int snd_pcm_hw_params_get_channels(const void *p,unsigned *c) { *c=2; return 0; }
int snd_pcm_hw_params_get_format(const void *p,int *f) { *f=2; return 0; }
void reset_reference(void) { accepted=0; }
long snd_pcm_writei(void *p,const void *b,unsigned long n) {
 const int16_t *s=b;
 double rate=atoi(getenv("RATE"));
 for(unsigned i=0;i<n;i++) {
   double gain=1;
   if(phase==1) gain=fmax(0.2,1.0-0.8*(accepted+i+1)/(rate*0.08));
   if(phase==2) gain=fmin(1.0,0.2+0.8*(accepted+i+1)/(rate*0.3));
   if(abs(s[i*2]-(int)(30000*gain))>1) fprintf(stderr,"phase=%d accepted=%u i=%u got=%d expected=%d\n",phase,accepted,i,s[i*2],(int)(30000*gain));
   assert(abs(s[i*2]-(int)(30000*gain))<=1);
   assert(abs(s[i*2+1]+(int)(30000*gain))<=1);
 }
 long result=failure ? -32 : short_write ? n/2 : n;
 if(result>0) accepted+=result;
 errno=123;
 return result;
}
long snd_pcm_mmap_writei(void *p,const void *b,unsigned long n) { return snd_pcm_writei(p,b,n); }
''')
            (work/'main.c').write_text(r'''
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdlib.h>
#include <unistd.h>
#include <time.h>
long snd_pcm_writei(void *,const void *,unsigned long);
long snd_pcm_mmap_writei(void *,const void *,unsigned long);
extern int phase, failure, short_write;
void reset_reference(void);
static void busy(int fd,int enabled) {
 struct timespec now; clock_gettime(CLOCK_MONOTONIC,&now);
 uint32_t ms=(uint32_t)((uint64_t)now.tv_sec*1000+now.tv_nsec/1000000);
 uint32_t deadline=enabled==1 ? ms+60000 : enabled==2 ? ms-1 : 0;
 assert(pwrite(fd,&deadline,4,0)==4);
}
int main(void) {
 int fd=open(getenv("SPOTIYA_DUCKING_STATE"),O_RDWR); assert(fd>=0);
 int16_t data[12000]; for(int i=0;i<6000;i++){data[i*2]=30000;data[i*2+1]=-30000;}
 assert(snd_pcm_writei((void*)1,data,6000)==6000);
 busy(fd,1); phase=1; reset_reference();
 short_write=1; assert(snd_pcm_writei((void*)1,data,1200)==600); assert(errno==123);
 failure=1; assert(snd_pcm_mmap_writei((void*)1,data,1200)==-32); assert(errno==123);
 failure=short_write=0;
 for(int i=0;i<4;i++) assert(snd_pcm_mmap_writei((void*)1,data,6000)==6000);
 busy(fd,2); phase=2; reset_reference();
 for(int i=0;i<12;i++) assert(snd_pcm_writei((void*)1,data,6000)==6000);
 busy(fd,1); phase=1; reset_reference();
 for(int i=0;i<4;i++) assert(snd_pcm_writei((void*)1,data,6000)==6000);
 busy(fd,0); phase=2; reset_reference();
 for(int i=0;i<12;i++) assert(snd_pcm_writei((void*)1,data,6000)==6000);
 phase=0;
 assert(snd_pcm_writei((void*)1,data,6000)==6000);
 for(int i=0;i<6000;i++){assert(data[i*2]==30000);assert(data[i*2+1]==-30000);}
 /* An expired watchdog deadline must leave the audio at full volume. */
 struct timespec now; clock_gettime(CLOCK_MONOTONIC,&now);
 uint32_t expired=(uint32_t)((uint64_t)now.tv_sec*1000+now.tv_nsec/1000000-1);
 assert(pwrite(fd,&expired,4,0)==4);
 assert(snd_pcm_writei((void*)1,data,6000)==6000);
 close(fd); return 0;
}
''')
            def cc(*args):
                subprocess.run(['cc', *args], cwd=work, check=True, capture_output=True)
            cc('-shared','-fPIC','mock.c','-lm','-o','mock.so')
            cc('main.c','-L.','-l:mock.so','-Wl,-rpath,'+str(work),'-o','shairport-sync')
            cc('-shared','-fPIC',str(ROOT/'controls/airplay_tap.c'),'-ldl','-pthread','-o','tap.so')
            state=work/'ducking'
            for rate in (16000, 44100, 48000, 96000, 192000):
                with self.subTest(rate=rate):
                    state.write_bytes(b'\0'*4)
                    env=os.environ | {'LD_PRELOAD':str(work/'tap.so'),
                        'SPOTIYA_SPECTRUM_SOCKET':str(work/'missing.sock'),
                        'SPOTIYA_DUCKING_STATE':str(state), 'RATE':str(rate)}
                    subprocess.run([str(work/'shairport-sync')],env=env,check=True,timeout=3)
