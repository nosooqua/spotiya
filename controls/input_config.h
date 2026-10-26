/* One frame of a stock Quasar ring animation: red, green and blue levels for the six
 * segments, already colour corrected, and how long the frame stays on. */
struct ring_frame {
    unsigned char level[18];
    unsigned short ms;
};
struct ring_animation {
    const struct ring_frame *frames;
    unsigned count;
};
/* One frame of a stock clock animation in the driver's LED order (digit * 7 + A..G, then
 * PLUS1..4, MINUS, DEGREE), at full brightness; it is scaled to the clock's level. */
struct display_frame {
    unsigned char level[34];
    unsigned short ms;
};
struct display_animation {
    const struct display_frame *frames;
    unsigned count;
};
struct input_binding {
    unsigned code, action, repeat;
};
struct input_gesture {
    unsigned from, to, action;
};
struct input_config {
    const char *input, *ring_prefix, *display_prefix;
    unsigned alice, volume_up, volume_down;
    /* Stock animations for transport gestures; an empty one keeps the built-in effect. */
    struct ring_animation next, previous, pause, resume;
    struct display_animation clock_next, clock_previous, clock_pause;
    /* Alice's volume colours, one uniform frame per level from quietest to loudest. */
    struct ring_animation volume;
    const struct input_binding *bindings;
    const struct input_gesture *gestures;
    unsigned binding_count, gesture_count;
    unsigned gesture_ms, repeat_delay_ms, repeat_ms, volume_step;
    const char *input_name;
};
static struct input_config control_config = {"/dev/input/event0",
                                             "/sys/class/leds/mini2:",
                                             "/sys/class/leds/mini2:white:clock",
                                             582,
                                             115,
                                             114,
                                             {0, 0},
                                             {0, 0},
                                             {0, 0},
                                             {0, 0},
                                             {0, 0},
                                             {0, 0},
                                             {0, 0},
                                             {0, 0},
                                             0,
                                             0,
                                             0,
                                             0,
                                             250,
                                             450,
                                             180,
                                             5,
                                             0};
