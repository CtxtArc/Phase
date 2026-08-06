/* extern fn implementations for examples/counting_loop.phase (see spec
 * §3.9, §5.2). The compiler only ever generates *prototypes* for
 * `extern fn` -- this is the hand-written body.
 *
 * This is the M9 demo: it exists to prove a `while` loop whose condition
 * changes via a real `name = expr;` assignment (see spec §9, Milestone
 * M9) actually runs a bounded, real number of iterations and then stops
 * -- not just that it compiles.
 */

#include "counting_loop.gen.h"

#include <stdio.h>

void tick(int32_t n) {
    printf("[sim] tick: n=%d\n", n);
}
