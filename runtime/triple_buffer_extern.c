/* extern fn implementations for examples/triple_buffer.phase (see spec
 * §3.9, §5.2). Hand-written, program-specific glue: the compiler only
 * generates the prototypes.
 */

#include "triple_buffer.gen.h"

#include <stdio.h>

/* Frames are numbered as captures start; every sample carries its frame
 * number, so process() can check frames arrive in order and intact, and
 * show which of the three buffers each one landed in. */
static unsigned next_frame = 0;
static const void *seen[3];

static char buffer_label(const void *p) {
    for (int i = 0; i < 3; i++) {
        if (seen[i] == NULL) {
            seen[i] = p;
        }
        if (seen[i] == p) {
            return (char)('A' + i);
        }
    }
    return '?';
}

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
