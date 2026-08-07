/* extern fn implementations for examples/hw_uart_echo.phase (see spec
 * §3.9, §5.2). Unlike every other example's *_extern.c, this one is
 * freestanding -- no libc, no printf -- because it's cross-compiled for
 * a real ARM Cortex-M3 target (see runtime/hw/, M10).
 */

#include "hw_uart_echo.gen.h"
#include "mps2an385.h"

void hw_fill(uint8_t *dst, size_t dst_len) {
    /* Stands in for a real capture device -- same role device_capture()
     * plays in radio_pipeline_extern.c, just without printf available. */
    static const char msg[] = "PHASE";
    for (size_t i = 0; i < dst_len; i++) {
        dst[i] = (uint8_t)msg[i % (sizeof(msg) - 1)];
    }
}

void uart_send(uint8_t *src, size_t src_len) {
    for (size_t i = 0; i < src_len; i++) {
        while (UART0->STATE & UART_STATE_TXFULL) {
            /* real hardware poll: wait for FIFO space */
        }
        UART0->CTRL |= UART_CTRL_TX_EN;
        UART0->DATA = src[i];
    }
}
