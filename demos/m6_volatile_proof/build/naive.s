	.file	"naive_no_volatile.c"
	.text
	.section	.text.startup,"ax",@progbits
	.p2align 4
	.globl	main
	.type	main, @function
main:
.LFB0:
	.cfi_startproc
	movl	$2, UART_CONTROL(%rip)
	movl	UART_STATUS(%rip), %eax
	movl	%eax, UART_DATA(%rip)
	xorl	%eax, %eax
	ret
	.cfi_endproc
.LFE0:
	.size	main, .-main
	.globl	UART_DATA
	.bss
	.align 4
	.type	UART_DATA, @object
	.size	UART_DATA, 4
UART_DATA:
	.zero	4
	.globl	UART_CONTROL
	.align 4
	.type	UART_CONTROL, @object
	.size	UART_CONTROL, 4
UART_CONTROL:
	.zero	4
	.globl	UART_STATUS
	.align 4
	.type	UART_STATUS, @object
	.size	UART_STATUS, 4
UART_STATUS:
	.zero	4
	.ident	"GCC: (GNU) 16.1.1 20260625"
	.section	.note.GNU-stack,"",@progbits
