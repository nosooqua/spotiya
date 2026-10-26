/* Optional, process-local ALSA observation for Shairport Sync. Playback calls, return
 * values and samples are untouched; a full/missing visualization socket drops data.
 * This library is embedded in Spotiya and loaded only for its own AirPlay child. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

typedef long (*writer)(void *, const void *, unsigned long);
static writer real_write, real_mmap;
static int (*params_malloc)(void **), (*params_current)(void *, void *);
static void (*params_free)(void *);
static int (*get_rate)(const void *, unsigned *, int *);
static int (*get_channels)(const void *, unsigned *);
static int (*get_format)(const void *, int *);
static struct sockaddr_un address;
static int enabled;
static pthread_once_t once = PTHREAD_ONCE_INIT;
static pthread_key_t socket_key;
static void close_socket(void *value) {
    if (value) close((int)((intptr_t)value - 1));
}
static void *symbol(const char *modern, const char *legacy) {
    void *p = dlsym(RTLD_NEXT, modern);
    return p ? p : dlsym(RTLD_NEXT, legacy);
}
static void init(void) {
    real_write = (writer)dlsym(RTLD_NEXT, "snd_pcm_writei");
    real_mmap = (writer)dlsym(RTLD_NEXT, "snd_pcm_mmap_writei");
    params_malloc = dlsym(RTLD_NEXT, "snd_pcm_hw_params_malloc");
    params_current = dlsym(RTLD_NEXT, "snd_pcm_hw_params_current");
    params_free = dlsym(RTLD_NEXT, "snd_pcm_hw_params_free");
    get_rate = symbol("__snd_pcm_hw_params_get_rate", "snd_pcm_hw_params_get_rate");
    get_channels = symbol("__snd_pcm_hw_params_get_channels", "snd_pcm_hw_params_get_channels");
    get_format = symbol("__snd_pcm_hw_params_get_format", "snd_pcm_hw_params_get_format");
    const char *path = getenv("SPOTIYA_SPECTRUM_SOCKET");
    char exe[256] = {0};
    ssize_t n = readlink("/proc/self/exe", exe, sizeof exe - 1);
    /* Hooks inherit LD_PRELOAD, but must never become another source of PCM. */
    const char *name = n > 0 ? strrchr(exe, '/') : NULL;
    if (!name || strcmp(name + 1, "shairport-sync") || !path || strlen(path) >= sizeof address.sun_path)
        return;
    if (!params_malloc || !params_current || !params_free || !get_rate || !get_channels || !get_format)
        return;
    if (pthread_key_create(&socket_key, close_socket)) return;
    address.sun_family = AF_UNIX;
    strcpy(address.sun_path, path);
    enabled = 1;
}
static __thread struct {
    void *pcm;
    unsigned rate;
    int valid, fd, opened;
    time_t probe;
} state;
static void observe(void *pcm, const unsigned char *data, unsigned long frames) {
    if (!enabled || !frames) return;
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    if (state.pcm != pcm || state.probe != now.tv_sec) {
        state.probe = now.tv_sec;
        state.pcm = pcm;
        state.valid = 0;
        if (access(address.sun_path, W_OK)) return;
        void *params = NULL;
        if (params_malloc(&params) < 0) return;
        unsigned channels = 0, rate = 0;
        int format = -1, direction = 0;
        if (params_current(pcm, params) >= 0 && get_rate(params, &rate, &direction) >= 0 &&
            get_channels(params, &channels) >= 0 && get_format(params, &format) >= 0 &&
            channels == 2 && format == 2 && rate >= 16000 && rate <= 192000) {
            state.rate = rate;
            state.valid = 1; /* S16_LE stereo, explicitly selected in shairport.conf */
        }
        params_free(params);
    }
    if (!state.valid) return;
    if (!state.opened) {
        state.fd = socket(AF_UNIX, SOCK_DGRAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
        if (state.fd < 0) return;
        if (pthread_setspecific(socket_key, (void *)((intptr_t)state.fd + 1))) {
            close(state.fd);
            return;
        }
        state.opened = 1;
    }
    while (frames) {
        unsigned n = frames > 1024 ? 1024 : (unsigned)frames;
        unsigned char packet[4104] = {'S', 'P', 'V', 1};
        for (unsigned i = 0; i < 4; i++) packet[4 + i] = state.rate >> (i * 8);
        memcpy(packet + 8, data, n * 4);
        if (sendto(state.fd, packet, 8 + n * 4, MSG_DONTWAIT | MSG_NOSIGNAL,
                   (const struct sockaddr *)&address, sizeof address) < 0) {
            /* Try again next second. Never block the audio output for a visual effect. */
            state.valid = 0;
            break;
        }
        data += n * 4;
        frames -= n;
    }
}
static long forward(writer fn, void *pcm, const void *buffer, unsigned long frames) {
    if (!fn) return -ENOSYS;
    long result = fn(pcm, buffer, frames);
    int saved = errno;
    if (result > 0 && (unsigned long)result <= frames)
        observe(pcm, buffer, (unsigned long)result);
    errno = saved;
    return result;
}
long snd_pcm_writei(void *pcm, const void *buffer, unsigned long frames) {
    pthread_once(&once, init);
    return forward(real_write, pcm, buffer, frames);
}
long snd_pcm_mmap_writei(void *pcm, const void *buffer, unsigned long frames) {
    pthread_once(&once, init);
    return forward(real_mmap, pcm, buffer, frames);
}
