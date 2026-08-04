/* Must come before any system headers: exposes usleep() under -std=c11,
 * which otherwise hides POSIX extensions in strict mode. */
#define _POSIX_C_SOURCE 200809L

/* Demo runtime glue for examples/radio_pipeline.phase (see spec §5).
 *
 * This is deliberately NOT a general-purpose PHASE runtime -- it's the
 * hand-written implementation of the specific `extern fn`s that one demo
 * program declares (device_capture, fir_filter, device_playback), plus
 * the two generic sim-wait functions any program's `sync` can call into.
 * A different PHASE program would need its own matching runtime file;
 * the compiler only ever generates *prototypes* for `extern fn` (see
 * codegen_c), never bodies -- see spec §3.9.
 */

#include "radio_pipeline.gen.h"
#include "phase_runtime.h"

#include <math.h>
#include <stdio.h>
#include <time.h>

/* ---- sim_dma_wait / sim_device_wait -------------------------------------
 *
 * v0.1 keeps everything in one address space (spec §5.1), so there is no
 * real separate memory to wait on -- device_capture()/device_playback()
 * below already finish their work synchronously before returning. What
 * these functions simulate is the *timing*: a short, visible delay plus a
 * trace message, standing in for where a real driver would block on a
 * DMA-complete interrupt or a device-ready register. Swapping in a real
 * wait-for-completion here is the entire job of porting to real hardware
 * (spec §5.3) -- nothing in the generated code or the PHASE source needs
 * to change.
 */

static void simulated_transfer_delay(void) {
    struct timespec ts = { .tv_sec = 0, .tv_nsec = 2 * 1000 * 1000 }; /* 2ms */
    nanosleep(&ts, NULL);
}

void sim_dma_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)buf;
    printf("[sim] sim_dma_wait(\"%s\"): waiting for DMA transfer to complete (%zu bytes)...\n",
           entity_name, nbytes);
    simulated_transfer_delay();
    printf("[sim] sim_dma_wait(\"%s\"): DMA transfer complete\n", entity_name);
}

void sim_device_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)buf;
    printf("[sim] sim_device_wait(\"%s\"): waiting for device transfer to complete (%zu bytes)...\n",
           entity_name, nbytes);
    simulated_transfer_delay();
    printf("[sim] sim_device_wait(\"%s\"): device transfer complete\n", entity_name);
}

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
