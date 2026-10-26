#define _POSIX_C_SOURCE 200809L
#define BUTTON_TEST
#include "../input.c"
#include <assert.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

static char test_root[128];
static char test_ring_prefix[160], test_display_prefix[160];
#include "lights.h"
#include "display.h"
static void clock_path(char *path, size_t size, int i) {
    static const char *extra[] = {"PLUS1", "PLUS2", "PLUS3", "PLUS4", "MINUS", "DEGREE"};
    if (i < 28)
        snprintf(path, size, "%stest-clock%c%d/brightness", test_root, 'A' + i % 7, i / 7 + 1);
    else
        snprintf(path, size, "%stest-clock%s/brightness", test_root, extra[i - 28]);
}
static int clock_value(int i) {
    char path[256];
    clock_path(path, sizeof path, i);
    int fd = open(path, O_RDONLY);
    assert(fd >= 0);
    int value = ring_read(fd);
    close(fd);
    return value;
}
static void clock_frame(const unsigned char *frame) {
    for (int i = 0; i < DISPLAY_LEDS; i++) {
        char path[256];
        clock_path(path, sizeof path, i);
        int fd = open(path, O_WRONLY);
        assert(fd >= 0);
        assert(ring_write(fd, frame[i]));
        close(fd);
    }
}
static unsigned clock_digit(int place) {
    unsigned mask = 0;
    for (int i = 0; i < 7; i++)
        if (clock_value(place * 7 + i))
            mask |= 1u << i;
    return mask;
}
static void path_for(char *path, size_t size, int i) {
    const char *color = i < 6 ? "red" : i < 12 ? "green" : "blue";
    snprintf(path, size, "%stest-led-%s:ring%d/brightness", test_root, color, i % 6 + 1);
}
static int brightness(int i) {
    char path[256];
    path_for(path, sizeof path, i);
    int fd = open(path, O_RDONLY);
    assert(fd >= 0);
    int value = ring_read(fd);
    close(fd);
    return value;
}
static void native_frame(int value) {
    for (int i = 0; i < 18; i++) {
        char path[256];
        path_for(path, sizeof path, i);
        int fd = open(path, O_WRONLY);
        assert(fd >= 0);
        assert(ring_write(fd, value));
        close(fd);
    }
}
int main(void) {
    char temp[] = "/tmp/spotify-ring-test-XXXXXX";
    char *dir = mkdtemp(temp);
    assert(dir);
    snprintf(test_root, sizeof test_root, "%s/", dir);
    snprintf(test_ring_prefix, sizeof test_ring_prefix, "%stest-led-", test_root);
    snprintf(test_display_prefix, sizeof test_display_prefix, "%stest-clock", test_root);
    control_config.ring_prefix = test_ring_prefix;
    control_config.display_prefix = test_display_prefix;
    for (int i = 0; i < 18; i++) {
        char path[256];
        path_for(path, sizeof path, i);
        char *slash = strrchr(path, '/');
        *slash = 0;
        assert(!mkdir(path, 0700));
        *slash = '/';
        int fd = open(path, O_CREAT | O_RDWR, 0600);
        assert(fd >= 0);
        assert(ring_write(fd, 0));
        close(fd);
    }
    struct gesture gesture = {0};
    key_event(&gesture, 582, 1, 1);
    struct lights l = {0};
    lights_begin(&l, TOGGLE, 0, 50, 100, 1000000);
    assert(l.active);
    lights_tick(&l, 1060000);
    assert(brightness(6) == 80 && brightness(0) < 10);
    lights_tick(&l, 1400000);
    assert(!l.active);
    for (int i = 0; i < 18; i++)
        assert(brightness(i) == 0);
    lights_begin(&l, TOGGLE, 1, 50, 100, 2000000);
    lights_tick(&l, 2060000);
    assert(brightness(0) == 80 && brightness(6) == 26);
    lights_close(&l, 1);
    /* Stock animations play frame by frame, then leave the ring dark. */
    static const struct ring_frame sweep[] = {
        {{[0] = 200, [6] = 10}, 20},
        {{[1] = 150, [13] = 90}, 30},
    };
    static const struct ring_frame pause_frames[] = {{{[5] = 77}, 40}};
    control_config.next = (struct ring_animation){sweep, 2};
    control_config.pause = (struct ring_animation){pause_frames, 1};
    lights_begin(&l, NEXT, 0, 50, 100, 2500000);
    lights_tick(&l, 2500000);
    assert(brightness(0) == 200 && brightness(6) == 10 && brightness(1) == 0);
    assert(l.next_frame == 2520000);
    lights_tick(&l, 2520000);
    assert(brightness(0) == 0 && brightness(1) == 150 && brightness(13) == 90);
    assert(l.next_frame == 2550000);
    lights_tick(&l, 2550000);
    assert(!l.active);
    for (int i = 0; i < 18; i++)
        assert(brightness(i) == 0);
    lights_begin(&l, TOGGLE, 1, 50, 100, 2600000);
    lights_tick(&l, 2600000);
    assert(brightness(5) == 77 && brightness(0) == 0);
    lights_close(&l, 1);
    /* Without a stock animation for resume, the built-in effect still runs. */
    lights_begin(&l, TOGGLE, 0, 50, 100, 2700000);
    lights_tick(&l, 2760000);
    assert(brightness(6) == 80);
    lights_close(&l, 1);
    control_config.next = control_config.pause = (struct ring_animation){0, 0};
    /* Volume uses Alice's level colours: held, then faded out, the whole ring at once. */
    static const struct ring_frame levels_palette[] = {
        {{[0] = 10, [1] = 10, [2] = 10, [3] = 10, [4] = 10, [5] = 10}, 1000},
        {{[6] = 200, [7] = 200, [8] = 200, [9] = 200, [10] = 200, [11] = 200}, 1000},
        {{[12] = 90, [13] = 90, [14] = 90, [15] = 90, [16] = 90, [17] = 90}, 1000},
    };
    control_config.volume = (struct ring_animation){levels_palette, 3};
    lights_begin(&l, LOUDER, 0, 50, 100, 8000000);
    lights_tick(&l, 8000000);
    assert(brightness(6) == 200 && brightness(11) == 200 && brightness(0) == 0);
    assert(l.next_frame == 8550000);
    lights_tick(&l, 8550000 + 210000);
    assert(brightness(6) == 100 && brightness(11) == 100);
    lights_begin(&l, QUIETER, 0, 100, 100, 8800000); /* a new press restarts the hold */
    lights_tick(&l, 8800000);
    assert(brightness(12) == 90 && brightness(6) == 0);
    lights_tick(&l, 8800000 + 970000);
    assert(!l.active);
    for (int i = 0; i < 18; i++)
        assert(brightness(i) == 0);
    lights_begin(&l, QUIETER, 0, 0, 100, 10000000);
    lights_tick(&l, 10000000);
    assert(brightness(0) == 10 && brightness(6) == 0);
    lights_close(&l, 1);
    control_config.volume = (struct ring_animation){0, 0};
    control_config.ring_prefix = NULL;
    assert(!lights_open(&l));
    control_config.ring_prefix = test_ring_prefix;
    assert(brightness(0) == 0);
    lights_begin(&l, LOUDER, 0, 50, 100, 3000000);
    lights_tick(&l, 3060000);
    assert(brightness(6) == 80 && brightness(8) == 80 && brightness(9) == 0);
    lights_close(&l, 1);
    const unsigned char original_digits[] = {6, 91, 79, 102}; /* 12:34 */
    unsigned char original[DISPLAY_LEDS] = {0};
    for (int i = 0; i < 28; i++)
        if (original_digits[i / 7] & (1u << (i % 7)))
            original[i] = 22;
    original[28] = original[29] = 22;
    for (int i = 0; i < DISPLAY_LEDS; i++) {
        char path[256];
        clock_path(path, sizeof path, i);
        char *slash = strrchr(path, '/');
        *slash = 0;
        assert(!mkdir(path, 0700));
        *slash = '/';
        int fd = open(path, O_CREAT | O_RDWR, 0600);
        assert(fd >= 0);
        assert(ring_write(fd, original[i]));
        close(fd);
    }
    struct display display = {0};
    display_show(&display, 75, 1000000);
    assert(display.active);
    assert(!clock_digit(0) && !clock_digit(1) && clock_digit(2) == 7 && clock_digit(3) == 109);
    assert(clock_value(28) == 0 && clock_value(29) == 0);
    char colon_path[256];
    clock_path(colon_path, sizeof colon_path, 28);
    int colon = open(colon_path, O_RDWR);
    assert(colon >= 0);
    assert(ring_write(colon, 22));
    close(colon);
    display_tick(&display, 2000000);
    assert(display.active && clock_value(28) == 0);
    display_tick(&display, 3100000);
    assert(!display.active);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) == original[i]);
    display_show(&display, 0, 4000000);
    assert(clock_digit(3) == 63 && !clock_digit(2));
    display_show(&display, 100, 5000000);
    assert(clock_digit(1) == 6 && clock_digit(2) == 63 && clock_digit(3) == 63);
    display_tick(&display, 6100000);
    assert(display.active); /* timeout extended */
    display_close(&display);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) == original[i]);
    display_show(&display, 57, 7000000);
    char native_path[256];
    clock_path(native_path, sizeof native_path, 4);
    int native = open(native_path, O_RDWR);
    assert(native >= 0);
    assert(ring_write(native, 33));
    close(native);
    display_tick(&display, 7100000);
    assert(!display.active && clock_value(4) == 33);
    clock_frame(original);
    assert(display_is_clock(original));
    unsigned char invalid[DISPLAY_LEDS];
    memcpy(invalid, original, sizeof invalid);
    invalid[0] = 22;
    assert(!display_is_clock(invalid));
    const unsigned char levels[] = {0, 35, 65, 100};
    display_spectrum(&display, levels, 1, 10000000);
    assert(display.active && display.mode == DISPLAY_SPECTRUM);
    assert(clock_digit(0) == 0);
    assert(clock_value(7 + 3) == 22 && clock_value(7 + 0) == 0);
    assert(clock_value(14 + 6) == 20 && clock_value(14 + 0) == 0);
    assert(clock_digit(3) == 127);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) <= 32);
    display_show(&display, 75, 11000000);
    display_spectrum(&display, levels, 1, 12000000);
    assert(display.mode == DISPLAY_VOLUME && clock_digit(2) == 7 && clock_digit(3) == 109);
    display_tick(&display, 13100000);
    display_spectrum(&display, levels, 1, 13100000);
    assert(display.mode == DISPLAY_SPECTRUM && clock_digit(3) == 127);
    display_spectrum(&display, levels, 0, 13200000);
    assert(!display.active);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) == original[i]);
    display_spectrum(&display, levels, 1, 14000000);
    unsigned char next_clock[DISPLAY_LEDS] = {0};
    const unsigned char next_digits[] = {6, 111, 79, 109}; /* 19:35 */
    for (int i = 0; i < 28; i++)
        if (next_digits[i / 7] & (1u << (i % 7)))
            next_clock[i] = 22;
    clock_frame(next_clock);
    display_tick(&display, 14100000);
    assert(!display.active);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) == next_clock[i]);
    display_spectrum(&display, levels, 1, 14200000);
    assert(!display.active);
    display_spectrum(&display, levels, 1, 16000000);
    assert(display.active);
    const unsigned char silent[] = {0, 0, 0, 0};
    display_spectrum(&display, silent, 1, 16100000);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) == 0);
    display_spectrum(&display, levels, 0, 16200000);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) == next_clock[i]);
    next_clock[33] = 22;
    clock_frame(next_clock);
    display_spectrum(&display, levels, 1, 17000000);
    assert(!display.active && clock_value(33) == 22);
    /* Stock clock animations play over the time at its brightness, then restore it. */
    next_clock[33] = 0;
    clock_frame(next_clock);
    static const struct display_frame swipe[] = {{{[3] = 255}, 20}, {{[30] = 128}, 40}};
    static const struct display_animation swipe_animation = {swipe, 2};
    display_play(&display, &swipe_animation, 20000000);
    assert(display.active && display.mode == DISPLAY_ANIMATION);
    display_tick(&display, 20000000);
    assert(clock_value(3) == 22 && clock_value(0) == 0 && clock_value(7) == 0);
    assert(display.check_at == 20020000);
    display_spectrum(&display, levels, 1, 20010000);
    assert(clock_value(3) == 22); /* the spectrum waits for the animation */
    display_tick(&display, 20020000);
    assert(clock_value(3) == 0 && clock_value(30) == 11);
    display_tick(&display, 20060000);
    assert(!display.active);
    for (int i = 0; i < DISPLAY_LEDS; i++)
        assert(clock_value(i) == next_clock[i]);
    display_play(&display, NULL, 21000000);
    assert(!display.active);
    for (int i = 0; i < DISPLAY_LEDS; i++) {
        char path[256];
        clock_path(path, sizeof path, i);
        assert(!unlink(path));
        *strrchr(path, '/') = 0;
        assert(!rmdir(path));
    }
    lights_begin(&l, NEXT, 0, 50, 100, 4000000);
    lights_tick(&l, 4160000);
    assert(brightness(8) == 80 && brightness(7) == 24 && brightness(6) == 5);
    lights_begin(&l, PREV, 0, 50, 100, 4200000);
    lights_tick(&l, 4360000);
    assert(brightness(9) == 80 && brightness(10) == 24 && brightness(11) == 5);
    native_frame(17);
    lights_tick(&l, 4390000);
    assert(!l.active);
    for (int i = 0; i < 18; i++)
        assert(brightness(i) == 17);
    lights_begin(&l, TOGGLE, 0, 50, 100, 6000000);
    assert(!l.active);
    for (int i = 0; i < 18; i++)
        assert(brightness(i) == 17);
    native_frame(0);
    lights_begin(&l, QUIETER, 0, 0, 100, 7000000);
    lights_tick(&l, 7060000);
    assert(l.active && brightness(0) > 0 && brightness(6) == 0);
    lights_close(&l, 1);
    for (int i = 0; i < 18; i++) {
        char path[256];
        path_for(path, sizeof path, i);
        assert(!unlink(path));
        *strrchr(path, '/') = 0;
        assert(!rmdir(path));
    }
    assert(!rmdir(dir));
    puts("Ring effects, volume/spectrum priority, clock restore and native preemption passed.");
    return 0;
}
