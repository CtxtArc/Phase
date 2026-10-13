#ifndef PHASE_CHECK_H
#define PHASE_CHECK_H

/* Runtime ownership checking for `phase build --checked` (spec §6.7).
 *
 * The generated C calls these at every ownership-relevant point -- buffer
 * creation, move, sync, borrow/release, destroy, every call argument,
 * swap, and every scope exit -- and they maintain a *shadow* state per
 * buffer storage (keyed by its address, so it follows `swap` and borrows
 * automatically) along the path the program actually executes.
 *
 * This is deliberately independent of the static analyzer: it never
 * reasons about branches or loops, it just watches one concrete execution.
 * A program `phase check` accepted should never trip it; if one does, the
 * static analysis is unsound for that program. The fuzzer
 * (crates/fuzz) relies on exactly that.
 *
 * Buffers are tracked by storage (domain, size, borrows, destroyed) and
 * typestate values by their address (state, consumed). Struct entities
 * that are not typestate values are not tracked.
 *
 * On a violation the process prints "PHASE runtime check failed: ..." to
 * stderr and exits with status 70.
 */

#define PHASE_CHECK_EXIT_CODE 70

#include <stddef.h>

/* The checker only compares buffer *addresses*; it never reads or writes
 * the memory. Saying so stops GCC from warning that a not-yet-written
 * buffer is "used uninitialized" when its address is passed here. */
#if defined(__GNUC__) && !defined(__clang__) && __GNUC__ >= 10
#define PHASE_ADDR_ONLY(...) __attribute__((access(none, __VA_ARGS__)))
#else
#define PHASE_ADDR_ONLY(...)
#endif

enum { PHASE_DOM_RAM, PHASE_DOM_STACK, PHASE_DOM_DMA, PHASE_DOM_MMIO, PHASE_DOM_DEVICE };
enum { PHASE_ACC_NONE, PHASE_ACC_READ, PHASE_ACC_WRITE };
enum { PHASE_VIA_OWNER, PHASE_VIA_READ_BORROW, PHASE_VIA_WRITE_BORROW };

/* PHASE function entry/exit (borrow rules are per call frame) */
void phase_chk_enter(void);
void phase_chk_leave(void);

/* storage lifetime */
PHASE_ADDR_ONLY(1)
void phase_chk_create(const void *storage, size_t bytes, int domain, const char *name);
PHASE_ADDR_ONLY(1) void phase_chk_scope_end(const void *storage, const char *name);

/* domain transitions */
PHASE_ADDR_ONLY(1) void phase_chk_move(const void *p, int to, const char *name);
PHASE_ADDR_ONLY(1) void phase_chk_sync(const void *p, const char *name);
PHASE_ADDR_ONLY(1) void phase_chk_destroy(const void *p, const char *name);
PHASE_ADDR_ONLY(1) PHASE_ADDR_ONLY(2)
void phase_chk_swap(const void *a, const void *b, const char *a_name, const char *b_name);

/* borrows: phase_chk_borrow returns a handle id */
PHASE_ADDR_ONLY(1) int phase_chk_borrow(const void *p, int write, const char *name);
void phase_chk_release(int handle, const char *name);
void phase_chk_handle_use(int handle, const char *name);
void phase_chk_handle_end(int handle, const char *name);

/* calls */
/* `told_bytes`: the size the callee is told the buffer has (its declared
 * length, or the run-time length it is passed, times its element size). */
PHASE_ADDR_ONLY(1)
void phase_chk_arg(const void *p, size_t told_bytes, int via, int param_domain, int access,
                   const char *callee, const char *name);
void phase_chk_alias(const char *callee, int n, const void *const *ptrs, const int *read_only);
PHASE_ADDR_ONLY(1) void phase_chk_transfer(const void *p, int to, const char *callee, const char *name);
PHASE_ADDR_ONLY(1) void phase_chk_param_exit(const void *p, int domain, const char *fn, const char *name);

/* typestate values: `state` is "Machine<State>" */
PHASE_ADDR_ONLY(1) void phase_chk_ts_set(const void *v, const char *state, const char *name);
PHASE_ADDR_ONLY(1)
void phase_chk_ts_fresh(const void *v, const char *state, const char *initial, const char *name);
PHASE_ADDR_ONLY(1)
void phase_chk_ts_use(const void *v, const char *state, int consume, const char *user,
                      const char *name);
PHASE_ADDR_ONLY(1) void phase_chk_ts_end(const void *v, const char *name);

#endif /* PHASE_CHECK_H */
