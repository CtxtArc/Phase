/* extern fn implementations for examples/radio_pipeline.phase (see spec
 * §3.9, §5.2). The compiler only ever generates *prototypes* for
 * `extern fn` -- these are the hand-written bodies. Program-specific by
 * design: a different PHASE program needs its own
 * runtime/<stem>_extern.c. The two generic sim-wait functions any
 * program's `sync` calls into live in phase_runtime.c instead, since
 * those don't depend on any one program's types.
 */

#include "radio_pipeline.gen.h"

#include <math.h>
#include <stdio.h>

/* ---- device_capture: simulated ADC front end ---------------------------
 *
 * Fills the buffer with a synthetic signal (a sine wave plus a small
 * amount of deterministic "noise") standing in for a real ADC capture --
 * see spec §5.2, sim_adc.
 */
void device_capture(Sample *dst, size_t dst_len) {
    printf("[sim] device_capture: filling %zu samples\n", dst_len);
    const float pi = 3.14159265358979323846f;
    const float freq = 0.05f;   /* cycles per sample, arbitrary */
    const float noise_amp = 0.05f;
    for (size_t i = 0; i < dst_len; i++) {
        float signal = sinf(2.0f * pi * freq * (float)i);
        /* deterministic pseudo-noise, not real randomness -- keeps the
         * demo's output reproducible across runs */
        float noise = noise_amp * sinf((float)i * 12.9898f);
        dst[i].value = signal + noise;
    }
}

/* ---- fir_filter: a small real FIR low-pass filter -----------------------
 *
 * A plain 5-tap moving-average filter over `.value`. Real DSP code, not a
 * stub -- this is exactly the kind of trusted, opaque kernel `extern fn`
 * exists for (spec §3.9): PHASE proves the buffer handoff around it is
 * safe, and has no opinion on what the kernel itself computes.
 */
void fir_filter(Sample *in_, size_t in_len, Sample *out, size_t out_len) {
    printf("[sim] fir_filter: filtering %zu -> %zu samples\n", in_len, out_len);
    const int taps = 5;
    size_t n = in_len < out_len ? in_len : out_len;
    for (size_t i = 0; i < n; i++) {
        float sum = 0.0f;
        int count = 0;
        for (int k = -(taps / 2); k <= taps / 2; k++) {
            long idx = (long)i + k;
            if (idx >= 0 && (size_t)idx < in_len) {
                sum += in_[idx].value;
                count++;
            }
        }
        out[i].value = sum / (float)count;
    }
}

/* ---- device_playback: simulated DAC / output stage ----------------------
 *
 * Doesn't actually play audio anywhere -- reports summary statistics
 * standing in for "the signal was streamed out," which is enough to prove
 * the whole pipeline actually ran end to end on real data.
 */
void device_playback(Sample *src, size_t src_len) {
    float min = src_len > 0 ? src[0].value : 0.0f;
    float max = min;
    double sum_sq = 0.0;
    for (size_t i = 0; i < src_len; i++) {
        float v = src[i].value;
        if (v < min) min = v;
        if (v > max) max = v;
        sum_sq += (double)v * (double)v;
    }
    float rms = src_len > 0 ? (float)sqrt(sum_sq / (double)src_len) : 0.0f;
    printf("[sim] device_playback: streaming %zu samples (min=%.4f max=%.4f rms=%.4f)\n",
           src_len, min, max, rms);
}
