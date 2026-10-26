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
