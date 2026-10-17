/* Output for a program's C externs on a bare-metal target: the board's
 * console (UART0 on the MPS2 boards, which QEMU shows; USART2 on the
 * NUCLEO-F401RE, which the ST-LINK shows as a serial port at 115200
 * baud). Implemented per board in phase_runtime_hw.c. */
#ifndef PHASE_HW_H
#define PHASE_HW_H

#include <stdint.h>

void phase_hw_putc(char c);
void phase_hw_puts(const char *s);
void phase_hw_put_u32(uint32_t v);

#endif /* PHASE_HW_H */
