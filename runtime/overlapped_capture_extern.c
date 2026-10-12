/* extern fn implementations for examples/overlapped_capture.phase (see
 * spec §3.9, §5.2). Hand-written, program-specific glue: the compiler only
 * generates the prototypes.
 */

#include "overlapped_capture.gen.h"

#include <math.h>
#include <stdio.h>

/* ---- start_capture: begin a simulated DMA transfer ----------------------
 *
 * Declared `@RAM -> @DMA` in PHASE: from here until the matching sync, the
 * buffer belongs to the (simulated) DMA engine. The simulation writes the
 * samples immediately -- everything shares one address space (spec §5.2)
 * -- and the later sync supplies the transfer's timing.
 */
void start_capture(Sample *dst, size_t dst_len) {
    printf("[sim] start_capture: DMA transfer of %zu samples started\n", dst_len);
    const float pi = 3.14159265358979323846f;
    for (size_t i = 0; i < dst_len; i++) {
        dst[i].value = sinf(2.0f * pi * 0.03f * (float)i);
    }
}

/* ---- make_window: CPU work that overlaps the transfer ------------------- */
void make_window(Sample *w, size_t w_len) {
    printf("[sim] make_window: computing a %zu-point Hann window (DMA still in flight)\n", w_len);
    const float pi = 3.14159265358979323846f;
    for (size_t i = 0; i < w_len; i++) {
        w[i].value = 0.5f - 0.5f * cosf(2.0f * pi * (float)i / (float)(w_len - 1));
    }
}

void apply_window(Sample *x, size_t x_len, const Sample *w, size_t w_len) {
    size_t n = x_len < w_len ? x_len : w_len;
    printf("[sim] apply_window: windowing %zu samples\n", n);
    for (size_t i = 0; i < n; i++) {
        x[i].value *= w[i].value;
    }
}

void report(const Sample *x, size_t x_len) {
    float peak = 0.0f, energy = 0.0f;
    for (size_t i = 0; i < x_len; i++) {
        float v = fabsf(x[i].value);
        if (v > peak) peak = v;
        energy += x[i].value * x[i].value;
    }
    printf("[sim] report: %zu samples, peak=%.4f, energy=%.4f, edges=%.4f/%.4f\n",
           x_len, peak, energy, x[0].value, x[x_len - 1].value);
}
