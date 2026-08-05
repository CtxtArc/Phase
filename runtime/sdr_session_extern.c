/* extern fn implementations for examples/sdr_session.phase (see spec
 * §3.9, §5.2). Reuses the same real DSP logic as
 * runtime/radio_pipeline_extern.c for the capture/filter/playback trio,
 * plus new implementations of the status-packet typestate chain
 * (receive_packet -> decode -> validate -> handle). Program-specific by
 * design -- see runtime/radio_pipeline_extern.c's header comment.
 */

#include "sdr_session.gen.h"

#include <math.h>
#include <stdio.h>

/* ---- RF front end / capture-filter-playback -----------------------------
 * Identical in spirit to radio_pipeline_extern.c; duplicated rather than
 * shared because each program's generated header declares its own,
 * separately-typed `Sample` struct (see codegen_c) -- there's no single
 * shared type to write one implementation against.
 */

void device_capture(Sample *dst, size_t dst_len) {
    printf("[sim] device_capture: filling %zu samples\n", dst_len);
    const float pi = 3.14159265358979323846f;
    const float freq = 0.05f;
    const float noise_amp = 0.05f;
    for (size_t i = 0; i < dst_len; i++) {
        float signal = sinf(2.0f * pi * freq * (float)i);
        float noise = noise_amp * sinf((float)i * 12.9898f);
        dst[i].value = signal + noise;
    }
}

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

/* ---- status packet typestate chain ---------------------------------------
 * Typestate itself (Received/Decoded/Validated) is a compile-time-only
 * proof -- it has no runtime representation (see phase_pir's doc comment
 * on typestate erasure). At runtime a Packet is just its one real field,
 * `sequence`; these functions model a plausible pipeline around it
 * (assign an id, "decode" a checksum, "validate" range-checks it).
 */

Packet receive_packet(void) {
    static uint32_t next_sequence = 1;
    Packet p;
    p.sequence = next_sequence++;
    printf("[sim] receive_packet: sequence=%u (state: Received)\n", p.sequence);
    return p;
}

Packet decode(Packet p) {
    printf("[sim] decode: sequence=%u (state: Received -> Decoded)\n", p.sequence);
    return p;
}

Packet validate(Packet p) {
    printf("[sim] validate: sequence=%u (state: Decoded -> Validated)\n", p.sequence);
    return p;
}

void handle(Packet p) {
    printf("[sim] handle: sequence=%u (state: Validated) -- accepted\n", p.sequence);
}
