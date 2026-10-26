import concurrent.futures
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import time
import shutil

RECIPE = Path(__file__).resolve().parent
ROOT = Path(os.environ.get("SPOTIYA_AIRPLAY_BUILD", Path.home()/".cache/spotiya/airplay2-armv7-glibc223")).resolve()
ROOT.mkdir(parents=True,exist_ok=True)
PREFIX = ROOT / 'prefix'
TOOL = Path(os.environ.get("SPOTIYA_TOOLCHAIN", Path.home()/".cache/spotiya/gcc-linaro-6.5.0-2018.12-x86_64_arm-linux-gnueabihf"))
ALSA = Path(os.environ.get("SPOTIYA_ALSA_SYSROOT", Path.home()/".cache/spotiya/alsa-sysroot-glibc223"))
CROSS = str(TOOL / 'bin/arm-linux-gnueabihf-')
FLAGS = '-Os -fPIC -ffunction-sections -fdata-sections -march=armv7-a -mfpu=neon -mfloat-abi=hard'
ENV = os.environ | {
    'PATH': os.environ['PATH'],
    'CC': CROSS+'gcc', 'CXX': CROSS+'g++', 'AR': CROSS+'ar',
    'RANLIB': CROSS+'ranlib', 'STRIP': CROSS+'strip',
    'CFLAGS': FLAGS, 'CXXFLAGS': FLAGS,
    'CPPFLAGS': f'-I{PREFIX}/include -I{ALSA}/usr/include',
    'LDFLAGS': f'-L{PREFIX}/lib -L{ALSA}/usr/lib -Wl,--gc-sections,--as-needed',
    'PKG_CONFIG_LIBDIR': f'{PREFIX}/lib/pkgconfig:{PREFIX}/share/pkgconfig:{ALSA}/usr/lib/pkgconfig',
    'PKG_CONFIG_PATH': '', 'PKG_CONFIG': str(ROOT/'pkg-config-static'),
    'ac_cv_func_malloc_0_nonnull': 'yes', 'ac_cv_func_realloc_0_nonnull': 'yes',
}
(ROOT/'logs').mkdir(exist_ok=True)
PREFIX.mkdir(exist_ok=True)
wrapper = ROOT/'pkg-config-static'
wrapper.write_text('#!/bin/sh\nexec /usr/bin/pkg-config --static "$@"\n')
wrapper.chmod(0o755)

def run(name, args, env=None, cwd=None):
    with (ROOT/'logs'/f'{name}.log').open('a') as log:
        log.write('\nCOMMAND: ' + repr(args) + '\n'); log.flush()
        subprocess.run(args, cwd=cwd or ROOT/name, env=ENV | (env or {}), stdout=log, stderr=subprocess.STDOUT, check=True)

def auto(name, opts=(), env=None):
    run(name, ['./configure', '--host=arm-linux-gnueabihf', '--build=x86_64-pc-linux-gnu', f'--prefix={PREFIX}', '--libdir='+str(PREFIX/'lib'), '--disable-shared', '--enable-static', *opts], env)
    run(name, ['make', '-j3'], env)
    if name in ('dbus', 'avahi'):
        stage = ROOT/'install-stage'/name
        run(name, ['make', 'install', 'DESTDIR='+str(stage)], env)
        subtree = stage / str(PREFIX).lstrip('/')
        for source in subtree.rglob('*'):
            target = PREFIX/source.relative_to(subtree)
            if source.is_symlink() and target.is_symlink(): target.unlink()
        shutil.copytree(subtree, PREFIX, dirs_exist_ok=True, symlinks=True)
    else:
        run(name, ['make', 'install'], env)

def build(name):
    stamp = ROOT/'logs'/f'{name}.done'
    signature=hashlib.sha256(b''.join((RECIPE/name).read_bytes() for name in ['build.py','fetch.py','sources.json','avahi-no-reverse.patch','avahi-runtime-user.patch','shairport-control.patch','spotiya-control.c','spotiya-control.h']) + str(TOOL).encode() + subprocess.check_output([CROSS+'gcc','--version'])).hexdigest()
    if stamp.exists() and stamp.read_text().strip()==signature:
        print(f'CACHED {name}', flush=True); return
    print(f'START {name}', flush=True)
    if name == 'openssl':
        run(name, ['perl', 'Configure', 'linux-armv4', '--cross-compile-prefix='+CROSS, f'--prefix={PREFIX}', '--libdir=lib', 'no-shared', 'no-tests', 'no-module', 'no-dso', 'no-engine', 'no-legacy', *FLAGS.split()], {'CC':'gcc','AR':'ar','RANLIB':'ranlib'})
        run(name, ['make', '-j4', 'build_libs'], {'CC':'gcc','AR':'ar','RANLIB':'ranlib'})
        run(name, ['make', 'install_dev'], {'CC':'gcc','AR':'ar','RANLIB':'ranlib'})
    elif name == 'ffmpeg':
        run(name, ['./configure', f'--prefix={PREFIX}', '--enable-cross-compile', '--target-os=linux', '--arch=arm', '--cpu=cortex-a7', '--cross-prefix='+CROSS, '--cc='+CROSS+'gcc', '--disable-everything', '--disable-autodetect', '--disable-network', '--disable-programs', '--disable-doc', '--disable-debug', '--disable-shared', '--enable-static', '--enable-small', '--enable-avcodec', '--enable-avformat', '--enable-avutil', '--enable-swresample', '--disable-avdevice', '--disable-avfilter', '--disable-swscale', '--disable-postproc', '--enable-decoder=aac,alac,pcm_s16be', '--enable-parser=aac', '--extra-cflags='+FLAGS, '--extra-ldflags=-Wl,--gc-sections'])
        run(name, ['make', '-j4']); run(name, ['make', 'install'])
    elif name == 'expat':
        run(name, ['cmake', '-S', '.', '-B', 'build-arm', '-G', 'Ninja', '-DCMAKE_SYSTEM_NAME=Linux', '-DCMAKE_SYSTEM_PROCESSOR=arm', '-DCMAKE_C_COMPILER='+CROSS+'gcc', '-DCMAKE_INSTALL_PREFIX='+str(PREFIX), '-DCMAKE_INSTALL_LIBDIR=lib', '-DCMAKE_C_FLAGS='+FLAGS, '-DBUILD_SHARED_LIBS=OFF', '-DEXPAT_BUILD_DOCS=OFF', '-DEXPAT_BUILD_EXAMPLES=OFF', '-DEXPAT_BUILD_TESTS=OFF', '-DEXPAT_BUILD_TOOLS=OFF', '-DEXPAT_SHARED_LIBS=OFF'])
        run(name, ['cmake', '--build', 'build-arm', '-j3']); run(name, ['cmake', '--install', 'build-arm'])
    elif name == 'gpgerror':
        auto(name, ['--disable-doc', '--disable-tests', '--disable-languages', '--disable-nls'])
    elif name == 'gcrypt':
        auto(name, ['--disable-doc', '--disable-tests', '--with-libgpg-error-prefix='+str(PREFIX), '--enable-ciphers=aes,chacha20', '--enable-pubkey-ciphers=rsa,ecc', '--enable-digests=md5,sha1,sha256,sha512,sha3,blake2', '--disable-padlock-support'])
    elif name == 'sodium':
        auto(name, ['--disable-pie'])
    elif name == 'plist':
        auto(name, ['--without-cython', '--without-tests'])
    elif name == 'config':
        auto(name, ['--disable-cxx', '--disable-examples'])
    elif name == 'popt':
        auto(name, ['--disable-nls'])
    elif name == 'daemon':
        auto(name, ['--disable-lynx', '--disable-examples'], {'ac_cv_func_setpgrp_void': 'yes'})
    elif name == 'uuid':
        auto(name, ['--disable-all-programs', '--enable-libuuid', '--disable-nls', '--without-python', '--disable-year2038'])
    elif name == 'dbus':
        auto(name, ['--disable-tests', '--disable-xml-docs', '--disable-doxygen-docs', '--disable-ducktype-docs', '--disable-selinux', '--disable-apparmor', '--disable-libaudit', '--disable-systemd', '--disable-x11-autolaunch', '--without-x', '--disable-verbose-mode', '--disable-asserts', '--disable-stats', '--disable-Werror', '--with-system-socket=/run/dbus/system_bus_socket', '--with-system-pid-file=/run/dbus/pid', '--sysconfdir=/etc', '--localstatedir=/var'])
    elif name == 'avahi':
        # Apply the shipped patch itself, so the package documents exactly what was built.
        for filename in ['avahi-no-reverse.patch', 'avahi-runtime-user.patch']:
            patch = str(RECIPE/filename)
            # --force: never guess the direction; --batch can reverse the wrong patch.
            applied = subprocess.run(['patch', '-p1', '--reverse', '--force', '--dry-run', '--silent', '-i', patch], cwd=ROOT/name, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
            if not applied:
                run(name, ['patch', '-p1', '--forward', '--force', '-i', patch])
        auto(name, ['--with-distro=none', '--disable-nls', '--disable-glib', '--disable-gobject', '--disable-introspection', '--disable-libevent', '--disable-qt3', '--disable-qt4', '--disable-qt5', '--disable-gtk', '--disable-gtk3', '--disable-gdbm', '--disable-python', '--disable-pygobject', '--disable-python-dbus', '--disable-mono', '--disable-monodoc', '--disable-autoipd', '--disable-doxygen-doc', '--disable-manpages', '--disable-xmltoman', '--disable-stack-protector', '--disable-ssp', '--with-xml=expat', '--with-dbus-sys=/etc/dbus-1/system.d', '--sysconfdir=/etc', '--localstatedir=/var', '--with-avahi-user=nobody', '--with-avahi-group=nogroup', '--with-systemdsystemunitdir=/nonexistent'])
    elif name == 'nqptp':
        run(name, ['autoreconf', '-fi'])
        run(name, ['./configure', '--host=arm-linux-gnueabihf', '--build=x86_64-pc-linux-gnu', f'--prefix={PREFIX}', '--sysconfdir=/etc', '--without-systemd-startup'])
        run(name, ['make', '-j3'])
    elif name == 'shairport-sync':
        patch = str(RECIPE/'shairport-control.patch')
        applied = subprocess.run(['patch', '-p1', '--reverse', '--force', '--dry-run', '--silent', '-i', patch], cwd=ROOT/name, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
        if not applied:
            run(name, ['patch', '-p1', '--forward', '--force', '-i', patch])
        for filename in ['spotiya-control.c', 'spotiya-control.h']:
            shutil.copy2(RECIPE/filename, ROOT/name/filename)
        source = ROOT/'shairport-sync/shairport.c'
        text = source.read_text()
        old = 'config.model = strdup("ShairportSync");'
        new = 'config.model = strdup("AudioAccessory5,1");'
        if old in text:
            assert text.count(old) == 1
            source.write_text(text.replace(old, new))
        elif not any(line.strip() == new for line in text.splitlines()):
            raise RuntimeError('Unexpected Shairport model source')
        modern_alsa = ['get_access', 'get_buffer_size', 'get_buffer_time', 'get_channels',
                       'get_format', 'get_period_size', 'get_period_time', 'get_periods',
                       'get_rate', 'get_subformat', 'set_buffer_size_near',
                       'set_period_size_near', 'set_rate_near']
        ENV['CPPFLAGS'] += ''.join(' -Dsnd_pcm_hw_params_'+n+'=__snd_pcm_hw_params_'+n for n in modern_alsa)
        ENV['LDFLAGS'] += ' -Wl,--disable-new-dtags,-rpath,/system/vendor/quasar/libs'
        run(name, ['autoreconf', '-fi'])
        run(name, ['./configure', '--host=arm-linux-gnueabihf', '--build=x86_64-pc-linux-gnu', f'--prefix={PREFIX}', '--sysconfdir=/etc', '--with-airplay-2', '--with-alsa', '--with-avahi', '--with-ssl=openssl', '--without-soxr', '--without-metadata', '--without-dbus-interface', '--with-spotiya-control', '--without-mpris-interface', '--without-mqtt-client', '--without-systemd-startup', '--without-libdaemon'], {'LIBS': '-lavahi-common -ldbus-1 -lexpat -lpthread -ldl -lm'})
        run(name, ['make', 'clean'])
        run(name, ['make', '-j4'])
    else:
        raise ValueError(name)
    stamp.write_text(signature+'\n')
    print(f'DONE {name}', flush=True)

if __name__ == '__main__':
    import fetch
    fetch.prepare(ROOT, RECIPE)
    if len(sys.argv)>1:
        for name in sys.argv[1:]: build(name)
    else:
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            list(pool.map(build, ['popt','config','gpgerror','expat','daemon','uuid','plist','sodium','openssl','ffmpeg','nqptp']))
        build('gcrypt'); build('dbus'); build('avahi'); build('shairport-sync')
