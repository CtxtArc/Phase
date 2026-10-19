/* The bare-metal runtime: what generated code and C externs call on a
 * board, per board.
 *
 * sim_dma_wait / sim_device_wait implement `sync` (declared in
 * phase_runtime.h; the host build simulates them). PHASE does not yet
 * know which DMA engine a buffer was handed to, so on a board these wait
 * on a stand-in -- the console's transmitter draining -- rather than the
 * engine's own completion flag. A program that must know a transfer is
 * done uses `wait E for buf` with the engine's interrupt, which is exact.
 * Tying `sync` to the engine is on the list for 3.0 (docs/real_boards.md).
 *
 * phase_hw_putc / phase_hw_puts / phase_hw_put_u32 (phase_hw.h) write to
 * the board's console. On the NUCLEO, phase_board_init and
 * phase_board_exit (hooks in startup.c) set the console up before `main`
 * and end the program after it.
 */
#include "phase_hw.h"
#include "phase_runtime.h"

#if defined(PHASE_BOARD_NUCLEO_F401RE) || defined(PHASE_BOARD_NUCLEO_F103RB)
#if defined(PHASE_BOARD_NUCLEO_F103RB)
#include "nucleo_f103rb.h"
#else
#include "nucleo_f401re.h"
#endif

/* USART2 on PA2 (TX) / PA3 (RX): the ST-LINK's virtual COM port, the same
 * on both Nucleo-64 boards. The F401RE runs from its 16 MHz HSI, the F103RB
 * from its 8 MHz one, as they start. */
void phase_board_init(void) {
    DBGMCU_CR |= DBGMCU_CR_DBG_ALL_LOW_POWER; /* debugger survives the final wfi */
#if defined(PHASE_BOARD_NUCLEO_F103RB)
    RCC_APB2ENR |= RCC_APB2ENR_IOPAEN;
    RCC_APB1ENR |= RCC_APB1ENR_USART2EN;
    (void)RCC_APB1ENR; /* the clock is on before the next access */
    /* PA2: AF push-pull 2 MHz (0xA), PA3: floating input (0x4). */
    GPIOA_CRL = (GPIOA_CRL & ~(0xFFu << 8)) | (0x4Au << 8);
#else
    RCC_AHB1ENR |= RCC_AHB1ENR_GPIOAEN;
    RCC_APB1ENR |= RCC_APB1ENR_USART2EN;
    (void)RCC_APB1ENR; /* the clock is on before the next access */
    GPIOA_MODER = (GPIOA_MODER & ~(0xFu << 4)) | (0xAu << 4); /* PA2, PA3: AF */
    GPIOA_AFRL = (GPIOA_AFRL & ~(0xFFu << 8)) | (0x77u << 8); /* AF7 */
#endif
    USART2_BRR = USART_BRR_115200_HSI;
    USART2_CR1 = USART_CR1_TE | USART_CR1_RE | USART_CR1_UE;
}

void phase_hw_putc(char c) {
    while (!(USART2_SR & USART_SR_TXE)) {
    }
    USART2_DR = (uint32_t)(unsigned char)c;
}

static void wait_for_console_drain(void) {
    while (!(USART2_SR & USART_SR_TC)) {
    }
}

/* After `main`: say so on the console -- `phase build --run` waits for
 * this line -- then stop. With a debugger attached, a semihosting exit
 * stops the core where the debugger sees it; without one that BKPT would
 * fault, so the core just sleeps. */
void phase_board_exit(void) {
    phase_hw_puts("\n[phase] main returned\n");
    wait_for_console_drain();
    if (DHCSR & DHCSR_C_DEBUGEN) {
        register uint32_t op __asm__("r0") = 0x18u;
        register uint32_t arg __asm__("r1") = 0x20026u;
        __asm__ volatile("bkpt 0xAB" : : "r"(op), "r"(arg) : "memory");
    }
    for (;;) {
        __asm__ volatile("wfi");
    }
}

#else /* the MPS2 boards on QEMU */

#if defined(PHASE_BOARD_MPS2_AN505)
#include "mps2an505.h"
#else
#include "mps2an385.h"
#endif

void phase_hw_putc(char c) {
    if (!(UART0->CTRL & UART_CTRL_TX_EN)) {
        UART0->CTRL |= UART_CTRL_TX_EN;
    }
    while (UART0->STATE & UART_STATE_TXFULL) {
    }
    UART0->DATA = (uint32_t)(unsigned char)c;
}

static void wait_for_console_drain(void) {
    /* Real register poll on real (QEMU-modeled) hardware -- an actual read
     * of a live status bit until the hardware itself reports it's done. */
    while (UART0->STATE & UART_STATE_TXFULL) {
    }
}

#endif

void phase_hw_puts(const char *s) {
    for (; *s; s++) {
#if defined(PHASE_BOARD_NUCLEO_F401RE) || defined(PHASE_BOARD_NUCLEO_F103RB)
        if (*s == '\n') {
            phase_hw_putc('\r'); /* a serial terminal wants CR LF */
        }
#endif
        phase_hw_putc(*s);
    }
}

void phase_hw_put_u32(uint32_t v) {
    char digits[10];
    int n = 0;
    do {
        digits[n++] = (char)('0' + v % 10u);
        v /= 10u;
    } while (v != 0);
    while (n > 0) {
        phase_hw_putc(digits[--n]);
    }
}

void sim_dma_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)entity_name;
    (void)buf;
    (void)nbytes;
    wait_for_console_drain();
}

void sim_device_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)entity_name;
    (void)buf;
    (void)nbytes;
    wait_for_console_drain();
}
