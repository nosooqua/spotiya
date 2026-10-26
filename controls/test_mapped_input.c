#define BUTTON_TEST
#include "input.c"
#include <assert.h>
#include <stdio.h>

static const struct input_binding bindings[] = {
    {582, TOGGLE, 0}, {115, LOUDER, 1}, {114, QUIETER, 1}};
static const struct input_gesture gestures[] = {{582, 115, NEXT}, {582, 114, PREV}};
struct recorded {
    int64_t at;
    unsigned code;
    int value;
};
static void replay(const struct recorded *events, size_t count, enum action expected) {
    struct mapped_input state = {0};
    queued = 0;
    for (size_t i = 0; i < count; i++)
        mapped_event(&state, events[i].code, events[i].value, events[i].at);
    mapped_tick(&state, events[count - 1].at + 1000000);
    assert(queued == 1 && queue[0] == expected);
    assert(mapped_deadline(&state) == 0);
}
int main(void) {
    /* Retain linkage of the legacy decoder in this standalone translation unit. */
    struct gesture legacy = {0};
    key_event(&legacy, 1, 0, 0);
    control_config.bindings = bindings;
    control_config.binding_count = 3;
    control_config.gestures = gestures;
    control_config.gesture_count = 2;
    /* Station 2 traces captured on 2026-10-26; relative kernel timestamps. */
    const struct recorded tap[] = {{0, 582, 1}, {255021, 582, 0}};
    const struct recorded plus[] = {{0, 115, 1}, {277818, 115, 0}};
    const struct recorded minus[] = {{0, 114, 1}, {257296, 114, 0}};
    const struct recorded right[] = {{0, 114, 1},      {66109, 582, 1},  {109145, 114, 0},
                                     {137824, 115, 1}, {162858, 582, 0}, {224719, 115, 0}};
    const struct recorded left[] = {{0, 115, 1},      {70972, 582, 1},  {115045, 115, 0},
                                    {133930, 114, 1}, {172659, 582, 0}, {216831, 114, 0}};
    const struct recorded centre_right[] = {
        {0, 582, 1}, {128522, 115, 1}, {178887, 582, 0}, {243541, 115, 0}};
    const struct recorded gap[] = {
        {0, 582, 1}, {100000, 582, 0}, {126000, 115, 1}, {180000, 115, 0}};
    replay(tap, 2, TOGGLE);
    replay(plus, 2, LOUDER);
    replay(minus, 2, QUIETER);
    replay(right, 6, NEXT);
    replay(left, 6, PREV);
    replay(centre_right, 4, NEXT);
    replay(gap, 4, NEXT);
    /* Cucumber awinic_sar capture, 2026-10-26: releases can precede
     * the next press in one scan; a swipe still produces just one action. */
    const struct recorded cucumber_tap[] = {{0, 582, 1}, {254727, 582, 0}};
    const struct recorded cucumber_plus[] = {{0, 115, 1}, {203424, 115, 0}};
    const struct recorded cucumber_minus[] = {{0, 114, 1}, {203848, 114, 0}};
    const struct recorded cucumber_right[] = {
        {0, 114, 1}, {101928, 582, 1}, {101959, 114, 0},
        {203844, 582, 0}, {203879, 115, 1}, {254696, 115, 0}};
    const struct recorded cucumber_left[] = {
        {0, 115, 1}, {101842, 582, 1}, {101870, 115, 0},
        {203868, 114, 1}, {254642, 582, 0}, {254674, 114, 0}};
    replay(cucumber_tap, 2, TOGGLE);
    replay(cucumber_plus, 2, LOUDER);
    replay(cucumber_minus, 2, QUIETER);
    replay(cucumber_right, 6, NEXT);
    replay(cucumber_left, 6, PREV);
    struct mapped_input state = {0};
    queued = 0;
    mapped_event(&state, 115, 1, 1000);
    assert(mapped_deadline(&state) == 451000);
    mapped_tick(&state, 450999);
    assert(queued == 0);
    mapped_tick(&state, 451000);
    assert(queued == 1 && queue[0] == LOUDER);
    mapped_event(&state, 115, 2, 500000);
    assert(queued == 1);
    mapped_tick(&state, 631000);
    assert(queued == 2);
    mapped_event(&state, 115, 0, 650000);
    mapped_tick(&state, 1000000);
    assert(queued == 2); /* no extra tap on release */
    mapped_reset(&state);
    queued = 0;
    mapped_event(&state, 582, 1, 0);
    mapped_event(&state, 582, 0, 100000);
    mapped_event(&state, 582, 1, 150000);
    mapped_event(&state, 582, 0, 200000);
    mapped_tick(&state, 1000000);
    assert(queued == 2);
    mapped_reset(&state);
    queued = 0;
    mapped_event(&state, 582, 0, 0);
    mapped_event(&state, 148, 1, 1000);
    mapped_event(&state, 148, 0, 2000);
    mapped_tick(&state, 1000000);
    assert(queued == 0);
    mapped_event(&state, 115, 1, 2000000);
    mapped_reset(&state);
    mapped_tick(&state, 3000000);
    assert(queued == 0); /* handover/dropped input */
    /* Configuration, not device-specific code, determines codes and actions. */
    const struct input_binding custom[] = {{200, NEXT, 0}, {201, QUIETER, 1}};
    const struct input_gesture custom_gesture[] = {{200, 201, TOGGLE}};
    control_config.bindings = custom;
    control_config.binding_count = 2;
    control_config.gestures = custom_gesture;
    control_config.gesture_count = 1;
    control_config.gesture_ms = 100;
    const struct recorded custom_trace[] = {
        {0, 200, 1}, {50000, 201, 1}, {80000, 200, 0}, {90000, 201, 0}};
    replay(custom_trace, 4, TOGGLE);
    puts("Mapped input: recorded Midi/Cucumber taps/swipes, hold, remapping and reset passed.");
    return 0;
}
