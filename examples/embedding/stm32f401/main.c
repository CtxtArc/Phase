/* The project's own C, written against ST's CMSIS header as usual, with
 * the PHASE driver's header next to it. PHASE_NO_SHORT_NAMES (set in the
 * Makefile) keeps the generated header to PHASE_-prefixed names, so the
 * two can't collide. */
#include "stm32f4xx.h"
#include "ticker.gen.h"

static void console_init(void) {
    RCC->AHB1ENR |= RCC_AHB1ENR_GPIOAEN;
    RCC->APB1ENR |= RCC_APB1ENR_USART2EN;
    (void)RCC->APB1ENR;
    GPIOA->MODER = (GPIOA->MODER & ~(GPIO_MODER_MODER2 | GPIO_MODER_MODER3)) |
                   (2u << GPIO_MODER_MODER2_Pos) | (2u << GPIO_MODER_MODER3_Pos);
    GPIOA->AFR[0] = (GPIOA->AFR[0] & ~0xFF00u) | 0x7700u; /* PA2, PA3: AF7 */
    USART2->BRR = (8u << USART_BRR_DIV_Mantissa_Pos) | 11u; /* 115200 at 16 MHz */
    USART2->CR1 = USART_CR1_TE | USART_CR1_RE | USART_CR1_UE;
}

static void console_puts(const char *s) {
    for (; *s; s++) {
        if (*s == '\n') {
            while (!(USART2->SR & USART_SR_TXE)) {
            }
            USART2->DR = '\r';
        }
        while (!(USART2->SR & USART_SR_TXE)) {
        }
        USART2->DR = (uint8_t)*s;
    }
}

/* ST's startup calls newlib's __libc_init_array, which calls _init; built
 * without the C runtime's own startup files (-nostartfiles), nothing else
 * defines it. */
void _init(void) {
}

int main(void) {
    console_init();
    console_puts("vendor main, PHASE ticker\n");
    ticker_start(10);
    for (int i = 1; i <= 5; i++) {
        ticker_wait();
        console_puts(i % 2 ? "tick\n" : "tock\n");
    }
    ticker_stop();
    console_puts("done\n");
    for (;;) {
        __WFI();
    }
}
