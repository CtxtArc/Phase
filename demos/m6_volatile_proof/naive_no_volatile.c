/* What a driver author might write BY HAND for this UART sequence,
 * forgetting that hardware registers must be marked `volatile`. This is a
 * real, extremely common embedded/driver bug class -- nothing about this
 * code *looks* wrong. Compare against phase_version.c, generated from
 * mmio_registers.phase by the actual PHASE compiler.
 */
#include <stdint.h>

uint32_t UART_STATUS;
uint32_t UART_CONTROL;
uint32_t UART_DATA;

int main(void) {
    uint32_t status = UART_STATUS;
    UART_CONTROL = 1;   /* e.g. "reset the UART" */
    UART_CONTROL = 2;   /* e.g. "enable the UART" */
    UART_DATA = status;
    return 0;
}
