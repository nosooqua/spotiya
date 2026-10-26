#include "ducking.h"
void spotiya_duck_f64(double *gain, double *samples, size_t frames, unsigned rate, int busy) {
    if (!busy && *gain == 1.0) return;
    for (size_t i = 0; i < frames; i++) {
        double level = spotiya_gain_next(gain, busy, rate);
        samples[i * 2] *= level;
        samples[i * 2 + 1] *= level;
    }
}
