/* The registers the NUCLEO-F401RE runtime itself uses (phase_runtime_hw.c):
 * the clocks, pins and USART2 behind the board's ST-LINK virtual COM port,
 * and the Cortex-M debug status. Addresses and bits from RM0368 and ST's
 * stm32f401xe.h; a PHASE program uses examples/devices/nucleo_f401re.phase
 * instead, which scripts/check_against_cmsis.sh checks against that header.
 */
#ifndef PHASE_HW_NUCLEO_F401RE_H
#define PHASE_HW_NUCLEO_F401RE_H

#include <stdint.h>

#define F4_REG(addr) (*(volatile uint32_t *)(addr))

#define RCC_AHB1ENR F4_REG(0x40023830u)
#define RCC_APB1ENR F4_REG(0x40023840u)
#define RCC_AHB1ENR_GPIOAEN (1u << 0)
#define RCC_APB1ENR_USART2EN (1u << 17)

#define GPIOA_MODER F4_REG(0x40020000u)
#define GPIOA_AFRL F4_REG(0x40020020u)

#define USART2_SR F4_REG(0x40004400u)
#define USART2_DR F4_REG(0x40004404u)
#define USART2_BRR F4_REG(0x40004408u)
#define USART2_CR1 F4_REG(0x4000440Cu)
#define USART_SR_TC (1u << 6)
#define USART_SR_TXE (1u << 7)
#define USART_CR1_TE (1u << 3)
#define USART_CR1_RE (1u << 2)
#define USART_CR1_UE (1u << 13)

/* 115200 baud from the 16 MHz HSI the chip starts on: USARTDIV = 8.6875. */
#define USART_BRR_115200_HSI ((8u << 4) | 11u)

/* Debug Halting Control and Status: C_DEBUGEN is set while a debugger
 * (the ST-LINK, under probe-rs or OpenOCD) is attached. */
/* DBGMCU_CR (RM0368 section 29.16): keep the debug port alive in
 * Sleep/Stop/Standby, so a `wfi` never cuts the ST-LINK off. */
#define DBGMCU_CR F4_REG(0xE0042004u)
#define DBGMCU_CR_DBG_ALL_LOW_POWER (0x7u)
#define DHCSR F4_REG(0xE000EDF0u)
#define DHCSR_C_DEBUGEN (1u << 0)

#endif /* PHASE_HW_NUCLEO_F401RE_H */
