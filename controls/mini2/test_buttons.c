#define BUTTON_TEST
#include "../input.c"
#include <assert.h>
#include <stdio.h>

struct recorded {
    int64_t at;
    unsigned code;
    int value;
};
static void replay(const struct recorded *events, size_t count, enum action expected) {
    struct gesture g = {0};
    queued = 0;
    for (size_t i = 0; i < count; i++)
        key_event(&g, events[i].code, events[i].value, events[i].at);
    tick(&g, events[count - 1].at + 1000000);
    assert(queued == 1 && queue[0] == expected);
}
int main(void) {
    /* Actual Mini 2 traces: overlapping sensors, release before arrival, 26ms gap. */
    const struct recorded next_overlap[] = {
        {240317, 582, 1}, {473913, 115, 1}, {473964, 582, 0}, {525313, 115, 0}};
    const struct recorded prev_release[] = {
        {146527, 582, 1}, {378154, 582, 0}, {378208, 114, 1}, {429659, 114, 0}};
    const struct recorded next_gap[] = {
        {691053, 582, 1}, {922581, 582, 0}, {948322, 115, 1}, {973993, 115, 0}};
    const struct recorded tap[] = {{346055, 582, 1}, {423140, 582, 0}};
    replay(next_overlap, 4, NEXT);
    replay(prev_release, 4, PREV);
    replay(next_gap, 4, NEXT);
    replay(tap, 2, TOGGLE);
    {
        struct gesture pending = {0};
        key_event(&pending, 582, 1, 1000);
        key_event(&pending, 582, 0, 2000);
        assert(gesture_deadline(&pending) == 2000 + GESTURE_US);
    }
    struct gesture g = {0};
    queued = 0;
    key_event(&g, 582, 1, 192947);
    key_event(&g, 582, 0, 321617);
    key_event(&g, 114, 1, 1117756);
    key_event(&g, 114, 0, 1220380);
    assert(queued == 2 && queue[0] == TOGGLE && queue[1] == QUIETER);
    memset(&g, 0, sizeof g);
    queued = 0;
    assert(gesture_deadline(&g) == 0);
    key_event(&g, 115, 1, 1000);
    assert(gesture_deadline(&g) == 451000);
    tick(&g, 450999);
    assert(queued == 1);
    tick(&g, 451000);
    assert(queued == 2 && queue[1] == LOUDER);
    assert(gesture_deadline(&g) == 451000 + REPEAT_US);
    key_event(&g, 115, 0, 452000);
    assert(gesture_deadline(&g) == 0);
    tick(&g, 999000);
    assert(queued == 2);
    memset(&g, 0, sizeof g);
    queued = 0;
    key_event(&g, 248, 1, 1000);
    key_event(&g, 248, 0, 2000);
    assert(queued == 0);
    control_config.alice = 200;
    control_config.volume_up = 201;
    control_config.volume_down = 202;
    const struct recorded remapped[] = {
        {1000, 200, 1}, {2000, 201, 1}, {3000, 200, 0}, {4000, 201, 0}};
    replay(remapped, 4, NEXT);
    assert(key_index(582) == -1 && key_index(115) == -1 && key_index(114) == -1);
    puts("Recorded gesture traces, separate taps, repeat, privacy key passed.");
    return 0;
}
