/* Model-independent EV_KEY decoder. Single taps are deferred so a full swipe
 * (edge -> centre -> edge) never changes volume before changing the track. */
struct mapped_input {
    unsigned char down[32];
    unsigned held, distinct, last;
    int consumed, repeated;
    int64_t pending, repeat_at;
};
static inline void mapped_reset(struct mapped_input *g) { memset(g, 0, sizeof *g); }
static inline void mapped_tick(struct mapped_input *g, int64_t now) {
    if (g->pending && now >= g->pending) {
        if (!g->consumed && !g->repeated && g->distinct == 1)
            emit((enum action)control_config.bindings[g->last].action);
        mapped_reset(g);
    }
    if (g->repeat_at && now >= g->repeat_at && g->held == 1 && g->distinct == 1 && !g->consumed) {
        emit((enum action)control_config.bindings[g->last].action);
        g->repeated = 1;
        g->repeat_at = now + (int64_t)control_config.repeat_ms * 1000;
    }
}
static inline int64_t mapped_deadline(const struct mapped_input *g) {
    return !g->pending                 ? g->repeat_at
           : !g->repeat_at             ? g->pending
           : g->pending < g->repeat_at ? g->pending
                                       : g->repeat_at;
}
static inline void mapped_event(struct mapped_input *g, unsigned code, int value, int64_t now) {
    mapped_tick(g, now);
    if (value != 0 && value != 1)
        return; /* Ignore kernel autorepeat. */
    unsigned index;
    for (index = 0; index < control_config.binding_count; index++)
        if (control_config.bindings[index].code == code)
            break;
    if (index == control_config.binding_count)
        return;
    if (!value) {
        if (!g->down[index])
            return;
        g->down[index] = 0;
        g->held--;
        g->repeat_at = 0;
        if (!g->held)
            g->pending = now + (int64_t)control_config.gesture_ms * 1000;
        return;
    }
    if (g->down[index])
        return;
    if (!g->held && g->pending && g->last == index) {
        /* Two distinct taps of the same button, not one long gesture. */
        mapped_tick(g, g->pending);
    }
    g->pending = 0;
    if (g->distinct && !g->consumed) {
        unsigned previous = control_config.bindings[g->last].code;
        for (unsigned i = 0; i < control_config.gesture_count; i++) {
            const struct input_gesture *gesture = &control_config.gestures[i];
            if (gesture->from == previous && gesture->to == code) {
                emit((enum action)gesture->action);
                g->consumed = 1;
                break;
            }
        }
    }
    g->down[index] = 1;
    g->held++;
    g->distinct++;
    g->last = index;
    g->repeat_at = g->distinct == 1 && control_config.bindings[index].repeat
                       ? now + (int64_t)control_config.repeat_delay_ms * 1000
                       : 0;
}
