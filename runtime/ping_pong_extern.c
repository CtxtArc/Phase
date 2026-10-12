/* extern fn implementations for examples/ping_pong.phase (see spec §3.9,
 * §5.2). Hand-written, program-specific glue: the compiler only generates
 * the prototypes.
 */

#include "ping_pong.gen.h"

#include <stdio.h>

/* Frames are numbered as captures start, and each sample carries its
 * frame number, so process() can check it received frames in order and
 * show which of the two buffers each one landed in. */
static unsigned next_frame = 0;
static const void *first_buffer = NULL;

static char buffer_label(const void *p) {
    if (first_buffer == NULL) {
        first_buffer = p;
    }
    return p == first_buffer ? 'A' : 'B';
}

/* Declared `@RAM -> @DMA`: from here until the matching sync the buffer
 * belongs to the (simulated) DMA engine. */
void start_capture(Sample *dst, size_t dst_len) {
    unsigned frame = next_frame++;
    for (size_t i = 0; i < dst_len; i++) {
        dst[i].value = (float)frame;
    }
    printf("[sim] start_capture: frame %u -> buffer %c\n", frame, buffer_label(dst));
}

void process(const Sample *frame, size_t frame_len) {
    static unsigned expected = 0;
    unsigned got = (unsigned)frame[0].value;
    int intact = 1;
    for (size_t i = 0; i < frame_len; i++) {
        if ((unsigned)frame[i].value != got) {
            intact = 0;
        }
    }
    printf("[sim] process: frame %u from buffer %c%s\n", got, buffer_label(frame),
           (got == expected && intact) ? "" : "  <-- OUT OF ORDER OR TORN");
    expected = got + 1;
}
