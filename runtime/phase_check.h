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
 * that are not typestate values are not tracked. Since v2.3, register
 * accesses are checked against their declared map, and interrupts against
 * where a `wait` may run and which writes a handler would lose.
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

/* rotate: each named storage must be live and unborrowed */
PHASE_ADDR_ONLY(1) void phase_chk_rotate(const void *p, const char *name);

/* split / join: `parts` are the parts' addresses, in order, each
 * `part_bytes` long */
PHASE_ADDR_ONLY(1)
void phase_chk_split(const void *parent, const char *name, int n, const void *const *parts,
                     const char *const *part_names, size_t part_bytes);
PHASE_ADDR_ONLY(1)
void phase_chk_join(const void *parent, const char *name, int n, const void *const *parts,
                    const char *const *part_names);

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

/* interrupts and registers (v2.3) */
enum { PHASE_REG_R = 1, PHASE_REG_W = 2 };
/* main returning: interrupts must not be left off */
void phase_chk_main_exit(void);
void phase_chk_handler_enter(void);
void phase_chk_handler_leave(void);
void phase_chk_critical_enter(void);
void phase_chk_critical_leave(void);
/* `wait E`: not in a handler, not inside `critical` */
void phase_chk_wait(const char *event);
/* a `wait` no handler satisfied */
void phase_chk_wait_stuck(const char *event);
/* `wait E for b` in fn `fn`: E gives back one buffer per function */
void phase_chk_wait_for(const char *fn, const char *event, const char *buf);
/* register accesses, against the register's declared access (`access`:
 * PHASE_REG_R | PHASE_REG_W); a write's value must be at most `limit`
 * (the field's or the register's largest value) */
void phase_chk_reg_read(const char *reg, int access);
void phase_chk_reg_write(const volatile void *reg, const char *name, int access, int field,
                         unsigned long long value, unsigned long long limit);
/* A field write's read-modify-write, outside `critical` and handlers:
 * returns 1 if an interrupt could land between the read and the write
 * (the generated code then runs the handlers there); any handler write to
 * the same register before phase_chk_rmw_end() is a lost write. */
int phase_chk_rmw_begin(const volatile void *reg, const char *name);
void phase_chk_rmw_end(void);

#endif /* PHASE_CHECK_H */
