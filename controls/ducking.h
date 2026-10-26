#ifndef SPOTIYA_DUCKING_H
#define SPOTIYA_DUCKING_H
#include <stddef.h>

/* One envelope for both Spotify's floating-point PCM and AirPlay's S16 PCM. */
static inline double spotiya_gain_next(double *gain, int busy, unsigned rate) {
    double target = busy ? 0.2 : 1.0;
    double step = 0.8 / (rate * (target < *gain ? 0.08 : 0.3));
    if (target < *gain) {
        *gain -= step;
        if (*gain < target) *gain = target;
    } else {
        *gain += step;
        if (*gain > target) *gain = target;
    }
    return *gain;
}
void spotiya_duck_f64(double *gain, double *samples, size_t frames, unsigned rate, int busy);
#endif
