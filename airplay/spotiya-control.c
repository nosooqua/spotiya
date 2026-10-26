#define _GNU_SOURCE
/* Private bridge to Shairport's existing AirPlay 2 / DACP remote control.
 * The socket lives in Spotiya's root-only runtime directory. No network listener. */
#include "spotiya-control.h"
#include "remote/remote.h"
#include "dacp.h"
#include "rtsp.h"
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <unistd.h>

extern ssize_t ap2_event_send_simple_modern_media_remote_command(rtsp_conn_info *, unsigned int);
extern pthread_rwlock_t metadata_hub_re_lock;
static int listener = -1, started;
static pthread_t worker;

static void close_client(void *arg) { close(*(int *)arg); }
static void unlock_connection(void *unused) {
    (void)unused;
    pthread_rwlock_unlock(&principal_conn_lock);
}
static void handle(int fd, const char *request) {
    unsigned session = 0, command = 0;
    char tail;
    int status = !strcmp(request, "STATUS\n");
    if (!status && (sscanf(request, "COMMAND %u %u %c", &session, &command, &tail) != 2 ||
                   command > 5 || command == 3)) {
        send(fd, "ERROR command\n", 14, MSG_NOSIGNAL);
        return;
    }
    char reply[80];
    strcpy(reply, "ERROR unavailable\n");
    /* Read without invoking the hub's mutating epilog or nesting its lock. */
    pthread_rwlock_rdlock(&metadata_hub_re_lock);
    int paused = metadata_store.play_status == PS_PAUSED || metadata_store.play_status == PS_STOPPED;
    if (metadata_store.play_status == PS_NOT_AVAILABLE) paused = -1;
    int dacp = metadata_store.dacp_server_active;
    pthread_rwlock_unlock(&metadata_hub_re_lock);
    pthread_rwlock_rdlock(&principal_conn_lock);
    pthread_cleanup_push(unlock_connection, NULL);
    rtsp_conn_info *conn = principal_conn;
    int available = conn && ((conn->airplay_type == ap_2 && conn->event_channel_fd > 0) || dacp);
    unsigned current = conn ? (unsigned)conn->connection_number + 1 : 0;
    if (status) {
        snprintf(reply, sizeof reply, "STATE %u %d %d\n", current, paused, available);
    } else if (current == session && available) {
        int ok = 0;
        if (conn->airplay_type == ap_2) {
            ok = ap2_event_send_simple_modern_media_remote_command(conn, command) > 0;
        } else {
            static const char *commands[] = {"play", "pause", "playpause", NULL, "nextitem", "previtem"};
            char *body = NULL;
            ssize_t length = 0;
            int code = dacp_send_command(commands[command], &body, &length);
            free(body);
            ok = code >= 200 && code < 300;
        }
        if (ok) strcpy(reply, "OK\n");
    }
    pthread_cleanup_pop(1);
    send(fd, reply, strlen(reply), MSG_NOSIGNAL);
}
static void *serve(void *unused) {
    (void)unused;
    for (;;) {
        int fd = accept4(listener, NULL, NULL, SOCK_CLOEXEC);
        if (fd < 0) {
            if (errno == EINTR) continue;
            return NULL;
        }
        pthread_cleanup_push(close_client, &fd);
        struct timeval timeout = {.tv_sec = 0, .tv_usec = 200000};
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof timeout);
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, sizeof timeout);
        char request[80];
        size_t size = 0;
        while (size < sizeof request - 1) {
            ssize_t n = recv(fd, request + size, 1, 0);
            if (n != 1) break;
            if (request[size++] == '\n') {
                request[size] = 0;
                handle(fd, request);
                break;
            }
        }
        pthread_cleanup_pop(1);
    }
}
void spotiya_control_start(void) {
    const char *path = getenv("SPOTIYA_AIRPLAY_CONTROL");
    struct sockaddr_un address = {.sun_family = AF_UNIX};
    if (!path || strlen(path) >= sizeof address.sun_path) return;
    strcpy(address.sun_path, path);
    listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (listener < 0) return;
    unlink(path);
    if (bind(listener, (struct sockaddr *)&address, sizeof address) || chmod(path, 0600) ||
        listen(listener, 4) || pthread_create(&worker, NULL, serve, NULL)) {
        close(listener);
        listener = -1;
        return;
    }
    started = 1;
}
void spotiya_control_stop(void) {
    if (!started) return;
    pthread_cancel(worker);
    pthread_join(worker, NULL);
    close(listener);
    listener = -1;
    started = 0;
}
