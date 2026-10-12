/* extern fn implementations for examples/multi_rate.phase (see spec §3.9,
 * §5.2). The `_len` arguments are the buffers' real lengths, passed by the
 * generated code -- including through PHASE helpers whose parameters are
 * `buffer<Sample, _>`.
 */

#include "multi_rate.gen.h"

#include <math.h>
#include <stdio.h>

void start_capture(Sample *dst, size_t dst_len) {
    printf("[sim] start_capture: %zu samples\n", dst_len);
    for (size_t i = 0; i < dst_len; i++) {
        dst[i].value = sinf((float)i * 0.1f);
    }
}

void summarize(const Sample *x, size_t x_len) {
    float energy = 0.0f;
    for (size_t i = 0; i < x_len; i++) {
        energy += x[i].value * x[i].value;
    }
    printf("[sim] summarize: %zu samples, energy %.2f\n", x_len, energy);
}

void decode_control(const Sample *x, size_t x_len) {
    printf("[sim] decode_control: %zu-sample control frame, first=%.3f\n", x_len, x[0].value);
}
