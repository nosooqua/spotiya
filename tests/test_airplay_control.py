"""Exercise the actual private Shairport socket bridge against stub sender transports."""
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]

@unittest.skipUnless(sys.platform.startswith('linux') and shutil.which('cc'), 'Linux socket bridge')
class AirplayControl(unittest.TestCase):
    def test_session_checks_transport_replies_and_shutdown(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            (work/'remote').mkdir()
            (work/'rtsp.h').write_text(r'''
#pragma once
#include <pthread.h>
#include <sys/types.h>
typedef struct { int connection_number, airplay_type, event_channel_fd; } rtsp_conn_info;
extern rtsp_conn_info *principal_conn;
extern pthread_rwlock_t principal_conn_lock;
enum { ap_2=2, PS_NOT_AVAILABLE, PS_PAUSED, PS_STOPPED, PS_PLAYING };
struct metadata_bundle { int play_status, dacp_server_active; };
extern struct metadata_bundle metadata_store;
#define metadata_hub_modify_prolog() ((void)0)
#define metadata_hub_modify_epilog(x) ((void)0)
''')
            (work/'remote/remote.h').write_text('#include "rtsp.h"\n')
            (work/'dacp.h').write_text('#include <sys/types.h>\nint dacp_send_command(const char *,char **,ssize_t *);\n')
            (work/'mock.c').write_text(r'''
#include "rtsp.h"
#include "spotiya-control.h"
#include <stdio.h>
static rtsp_conn_info conn={6,ap_2,1};
rtsp_conn_info *principal_conn=&conn;
pthread_rwlock_t principal_conn_lock=PTHREAD_RWLOCK_INITIALIZER;
pthread_rwlock_t metadata_hub_re_lock=PTHREAD_RWLOCK_INITIALIZER;
struct metadata_bundle metadata_store={PS_PLAYING,0};
ssize_t ap2_event_send_simple_modern_media_remote_command(rtsp_conn_info *c,unsigned int command) {
 if(command==4) return -1;
 if(command==0) metadata_store.play_status=PS_PLAYING;
 if(command==1) metadata_store.play_status=PS_PAUSED;
 return 1;
}
int dacp_send_command(const char *command,char **body,ssize_t *length) { return 500; }
int main(void) {
 spotiya_control_start(); puts("ready"); fflush(stdout);
 getchar(); spotiya_control_stop(); return 0;
}
''')
            subprocess.run(['cc','-Wall','-Wextra','-Werror','-Wno-unused-parameter','-pthread',
                '-I'+str(work),'-I'+str(ROOT/'airplay'),str(ROOT/'airplay/spotiya-control.c'),
                str(work/'mock.c'),'-o',str(work/'server')],check=True,capture_output=True)
            endpoint=str(work/'control')
            process=subprocess.Popen([str(work/'server')],env=os.environ | {'SPOTIYA_AIRPLAY_CONTROL':endpoint},
                stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            try:
                self.assertEqual(process.stdout.readline(),b'ready\n')
                self.assertEqual(Path(endpoint).stat().st_mode & 0o777,0o600)
                def request(command):
                    with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as client:
                        client.settimeout(2);client.connect(endpoint);client.sendall(command)
                        return client.recv(128)
                self.assertEqual(request(b'STATUS\n'),b'STATE 7 0 1\n')
                self.assertEqual(request(b'COMMAND 6 1\n'),b'ERROR unavailable\n')
                self.assertEqual(request(b'STATUS\n'),b'STATE 7 0 1\n')
                self.assertEqual(request(b'COMMAND 7 1\n'),b'OK\n')
                self.assertEqual(request(b'STATUS\n'),b'STATE 7 1 1\n')
                self.assertEqual(request(b'COMMAND 7 0\n'),b'OK\n')
                self.assertEqual(request(b'COMMAND 7 4\n'),b'ERROR unavailable\n')
                self.assertEqual(request(b'STATUS\n'),b'STATE 7 0 1\n')
                for command in (b'COMMAND 7 3\n',b'COMMAND 7 9\n',b'COMMAND 7 0 extra\n',b'invalid\n'):
                    self.assertEqual(request(command),b'ERROR command\n')
                process.communicate(b'\n',timeout=2)
                self.assertEqual(process.returncode,0)
            finally:
                if process.poll() is None: process.kill()
                process.communicate()
