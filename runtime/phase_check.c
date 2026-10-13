/* Runtime ownership checking for `phase build --checked`. See
 * phase_check.h for what this is and why it is independent of the static
 * analyzer.
 *
 * Buffers are identified by their storage address, converted to an
 * integer key (`Key`) as soon as it enters the checker: the checker never
 * touches buffer memory, and keeping raw pointers out of its internals
 * also keeps compilers from assuming it might read that memory.
 */

#include "phase_check.h"

#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_STORAGES 1024
#define MAX_HANDLES 4096
#define MAX_TSVALS 1024

typedef uintptr_t Key;

#define KEY(p) ((Key)(p))

typedef struct {
    Key key;
    const char *name;
    size_t bytes;
    int domain;
    int destroyed;
    /* Split into parts: set aside (lookups skip it -- its first part shares
     * its address) until the parts are joined back. */
    int split;
    int frame; /* the call depth it was split at */
    int in_use;
} Storage;

typedef struct {
    Key key;
    const char *name;
    const char *state;
    int consumed;
    int in_use;
} TsVal;

typedef struct {
    Key key;
    const char *name;
    int write;
    int released;
    int in_use;
    int depth; /* call depth the borrow was taken at */
} Handle;

static Storage storages[MAX_STORAGES];
static Handle handles[MAX_HANDLES];
static TsVal tsvals[MAX_TSVALS];

/* Call depth of the PHASE function currently running. Borrow rules are
 * per frame: a function only conflicts with borrows taken in its own
 * frame. A borrow held by a caller is what *grants* the callee its access
 * to the parameter -- the static rules already ensure a caller can only
 * hand down access it holds (a read borrow only to a `read` parameter). */
static int depth = 0;

void phase_chk_enter(void) { depth++; }
void phase_chk_leave(void) { depth--; }

static const char *dom_name(int d) {
    switch (d) {
    case PHASE_DOM_RAM: return "@RAM";
    case PHASE_DOM_STACK: return "@STACK";
    case PHASE_DOM_DMA: return "@DMA";
    case PHASE_DOM_MMIO: return "@MMIO";
    case PHASE_DOM_DEVICE: return "@DEVICE";
    default: return "@?";
    }
}

static void fail(const char *fmt, ...) {
    va_list ap;
    fflush(stdout);
    fprintf(stderr, "PHASE runtime check failed: ");
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fprintf(stderr, "\n");
    exit(PHASE_CHECK_EXIT_CODE);
}

static int device_owned(int d) { return d == PHASE_DOM_DMA || d == PHASE_DOM_DEVICE; }

/* Live borrows of storage `key`: in the current frame only, or in any. */
static void borrows_of(Key key, int any_frame, int *readers, int *writer) {
    *readers = 0;
    *writer = 0;
    for (int i = 0; i < MAX_HANDLES; i++) {
        const Handle *h = &handles[i];
        if (h->in_use && !h->released && h->key == key && (any_frame || h->depth == depth)) {
            if (h->write) {
                (*writer)++;
            } else {
                (*readers)++;
            }
        }
    }
}

/* The set-aside record of a buffer this frame split under `name`, if any.
 * The name matters: a split buffer's first part shares its address (and a
 * part can itself be split), so several records can sit at one key. A
 * split buffer cannot be swapped or rotated, so its name stays put. */
static Storage *find_split(Key key, const char *name) {
    for (int i = 0; i < MAX_STORAGES; i++) {
        if (storages[i].in_use && storages[i].split && storages[i].key == key &&
            storages[i].frame == depth && strcmp(storages[i].name, name) == 0) {
            return &storages[i];
        }
    }
    return NULL;
}

static Storage *find(Key key, const char *name) {
    for (int i = 0; i < MAX_STORAGES; i++) {
        if (storages[i].in_use && !storages[i].split && storages[i].key == key) {
            /* A split buffer's first part shares its address. If the name
             * used is the split buffer's own, it is being used whole. */
            if (find_split(key, name)) {
                fail("'%s' is split into parts; it cannot be used whole until they are joined",
                     name);
            }
            return &storages[i];
        }
    }
    fail("'%s' does not refer to a live buffer", name);
    return NULL;
}

static Storage *live(Key key, const char *name, const char *what) {
    Storage *s = find(key, name);
    if (s->destroyed) {
        fail("'%s' was destroyed; cannot %s", name, what);
    }
    return s;
}

static void no_borrows(const Storage *s, const char *name, const char *what) {
    int readers, writer;
    borrows_of(s->key, 0, &readers, &writer);
    if (readers || writer) {
        fail("'%s' has a live borrow; cannot %s", name, what);
    }
}

void phase_chk_create(const void *storage, size_t bytes, int domain, const char *name) {
    Key key = KEY(storage);
    for (int i = 0; i < MAX_STORAGES; i++) {
        if (storages[i].in_use && !storages[i].split && storages[i].key == key) {
            fail("buffer '%s' created over live storage of '%s'", name, storages[i].name);
        }
    }
    for (int i = 0; i < MAX_STORAGES; i++) {
        if (!storages[i].in_use) {
            storages[i] = (Storage){key, name, bytes, domain, 0, 0, 0, 1};
            return;
        }
    }
    fail("too many live buffers (limit %d)", MAX_STORAGES);
}

void phase_chk_scope_end(const void *storage, const char *name) {
    Key key = KEY(storage);
    /* A buffer still split at its end: its parts carried (and were checked
     * for) its storage, and have already gone out of scope. Its storage
     * ends under the name it was declared with, which `swap`/`rotate` may
     * have moved off it before the split -- so when no live buffer is left
     * at this address, the split record here is the one ending. */
    Storage *sp = find_split(key, name);
    if (!sp) {
        int live_here = 0;
        for (int i = 0; i < MAX_STORAGES; i++) {
            if (storages[i].in_use && !storages[i].split && storages[i].key == key) {
                live_here = 1;
            }
        }
        for (int i = 0; i < MAX_STORAGES && !live_here && !sp; i++) {
            if (storages[i].in_use && storages[i].split && storages[i].key == key &&
                storages[i].frame == depth) {
                sp = &storages[i];
            }
        }
    }
    if (sp) {
        sp->in_use = 0;
        return;
    }
    Storage *s = find(key, name);
    if (!s->destroyed && device_owned(s->domain)) {
        fail("'%s' goes out of scope while owned by %s", name, dom_name(s->domain));
    }
    int readers, writer;
    borrows_of(key, 1, &readers, &writer);
    if (readers || writer) {
        fail("'%s' goes out of scope with a live borrow", name);
    }
    s->in_use = 0;
}

void phase_chk_move(const void *p, int to, const char *name) {
    Storage *s = live(KEY(p), name, "move it");
    no_borrows(s, name, "move it");
    int from = s->domain;
    int legal = (from == PHASE_DOM_STACK && to == PHASE_DOM_RAM) ||
                (from == PHASE_DOM_RAM && (to == PHASE_DOM_DMA || to == PHASE_DOM_DEVICE));
    if (!legal) {
        fail("'%s': no legal move from %s to %s", name, dom_name(from), dom_name(to));
    }
    s->domain = to;
}

void phase_chk_sync(const void *p, const char *name) {
    Storage *s = live(KEY(p), name, "sync it");
    no_borrows(s, name, "sync it");
    if (!device_owned(s->domain)) {
        fail("'%s': sync from %s, which no hardware owns", name, dom_name(s->domain));
    }
    s->domain = PHASE_DOM_RAM;
}

void phase_chk_destroy(const void *p, const char *name) {
    Storage *s = live(KEY(p), name, "destroy it");
    no_borrows(s, name, "destroy it");
    if (device_owned(s->domain)) {
        fail("'%s' destroyed while owned by %s", name, dom_name(s->domain));
    }
    s->destroyed = 1;
}

void phase_chk_swap(const void *a, const void *b, const char *a_name, const char *b_name) {
    Storage *sa = live(KEY(a), a_name, "swap it");
    Storage *sb = live(KEY(b), b_name, "swap it");
    no_borrows(sa, a_name, "swap it");
    no_borrows(sb, b_name, "swap it");
}

void phase_chk_rotate(const void *p, const char *name) {
    Storage *s = live(KEY(p), name, "rotate it");
    no_borrows(s, name, "rotate it");
}

void phase_chk_split(const void *parent, const char *name, int n, const void *const *parts,
                     const char *const *part_names, size_t part_bytes) {
    Storage *s = live(KEY(parent), name, "split it");
    no_borrows(s, name, "split it");
    if (part_bytes * (size_t)n != s->bytes) {
        fail("'%s' (%zu bytes) split into %d parts of %zu bytes", name, s->bytes, n, part_bytes);
    }
    int domain = s->domain;
    s->split = 1;
    s->frame = depth;
    s->name = name; /* the record may carry the name of a buffer swapped away */
    for (int i = 0; i < n; i++) {
        if (KEY(parts[i]) != KEY(parent) + (Key)i * part_bytes) {
            fail("part '%s' of '%s' is not at its slice of the buffer", part_names[i], name);
        }
        phase_chk_create(parts[i], part_bytes, domain, part_names[i]);
    }
}

void phase_chk_join(const void *parent, const char *name, int n, const void *const *parts,
                    const char *const *part_names) {
    Key key = KEY(parent);
    Storage *s = find_split(key, name);
    if (!s) {
        fail("join into '%s', which is not split", name);
    }
    size_t part_bytes = s->bytes / (size_t)n;
    int domain = -1;
    for (int i = 0; i < n; i++) {
        /* Each slot must hold the original slice: a part swapped away
         * cannot be joined back in its place. */
        if (KEY(parts[i]) != key + (Key)i * part_bytes) {
            fail("'%s' does not hold slice %d of '%s'", part_names[i], i, name);
        }
        Storage *p = live(KEY(parts[i]), part_names[i], "join it");
        no_borrows(p, part_names[i], "join it");
        if (domain < 0) {
            domain = p->domain;
        } else if (p->domain != domain) {
            fail("joining '%s' while its parts are in %s and %s", name, dom_name(domain),
                 dom_name(p->domain));
        }
    }
    for (int i = 0; i < n; i++) {
        find(KEY(parts[i]), part_names[i])->in_use = 0;
    }
    s->split = 0;
    s->domain = domain;
}

int phase_chk_borrow(const void *p, int write, const char *name) {
    Key key = KEY(p);
    Storage *s = live(key, name, "borrow it");
    if (device_owned(s->domain)) {
        fail("borrow '%s' of a buffer owned by %s", name, dom_name(s->domain));
    }
    int readers, writer;
    borrows_of(key, 0, &readers, &writer);
    if (writer || (write && readers)) {
        fail("borrow '%s' conflicts with a live borrow of the same buffer", name);
    }
    for (int i = 0; i < MAX_HANDLES; i++) {
        if (!handles[i].in_use) {
            handles[i] = (Handle){key, name, write, 0, 1, depth};
            return i;
        }
    }
    fail("too many live borrows (limit %d)", MAX_HANDLES);
    return -1;
}

static Handle *handle(int h, const char *name) {
    if (h < 0 || h >= MAX_HANDLES || !handles[h].in_use) {
        fail("'%s' is not a live borrow", name);
    }
    return &handles[h];
}

void phase_chk_release(int h, const char *name) {
    Handle *hd = handle(h, name);
    if (hd->released) {
        fail("borrow '%s' released twice", name);
    }
    hd->released = 1;
}

void phase_chk_handle_use(int h, const char *name) {
    if (handle(h, name)->released) {
        fail("borrow '%s' used after release", name);
    }
}

void phase_chk_handle_end(int h, const char *name) {
    Handle *hd = handle(h, name);
    if (!hd->released) {
        fail("borrow '%s' goes out of scope without being released", name);
    }
    hd->in_use = 0;
}

void phase_chk_arg(const void *p, size_t told_bytes, int via, int param_domain, int access,
                   const char *callee, const char *name) {
    Key key = KEY(p);
    Storage *s = live(key, name, "pass it to a function");
    if (told_bytes != s->bytes) {
        fail("'%s' is %zu bytes, but '%s' is told it has %zu", name, s->bytes, callee,
             told_bytes);
    }
    if (param_domain >= 0 && s->domain != param_domain) {
        fail("'%s' is in %s but '%s' expects %s", name, dom_name(s->domain), callee,
             dom_name(param_domain));
    }
    int readers, writer;
    borrows_of(key, 0, &readers, &writer);
    switch (via) {
    case PHASE_VIA_OWNER:
        if (writer || (access != PHASE_ACC_READ && readers)) {
            fail("'%s' passed to '%s' while borrowed", name, callee);
        }
        break;
    case PHASE_VIA_READ_BORROW:
        if (access == PHASE_ACC_WRITE) {
            fail("read borrow '%s' passed to '%s', which writes it", name, callee);
        }
        break;
    default:
        break;
    }
}

void phase_chk_alias(const char *callee, int n, const void *const *ptrs, const int *read_only) {
    for (int i = 0; i < n; i++) {
        for (int j = i + 1; j < n; j++) {
            if (KEY(ptrs[i]) == KEY(ptrs[j]) && !(read_only[i] && read_only[j])) {
                fail("the same buffer is passed to '%s' twice, and not read-only both times",
                     callee);
            }
        }
    }
}

void phase_chk_transfer(const void *p, int to, const char *callee, const char *name) {
    Storage *s = live(KEY(p), name, "hand it to hardware");
    no_borrows(s, name, "hand it to hardware");
    int from = s->domain;
    int legal = (from == PHASE_DOM_STACK && to == PHASE_DOM_RAM) ||
                (from == PHASE_DOM_RAM && (to == PHASE_DOM_DMA || to == PHASE_DOM_DEVICE)) ||
                (device_owned(from) && to == PHASE_DOM_RAM);
    if (!legal) {
        fail("'%s' moves '%s' from %s to %s, which is not a legal transition", callee, name,
             dom_name(from), dom_name(to));
    }
    s->domain = to;
}

void phase_chk_param_exit(const void *p, int domain, const char *fn, const char *name) {
    Storage *s = find(KEY(p), name);
    if (!s->destroyed && s->domain != domain) {
        fail("parameter '%s' leaves '%s' in %s, but its signature says %s", name, fn,
             dom_name(s->domain), dom_name(domain));
    }
}

/* ---- typestate values ---------------------------------------------------
 *
 * A typestate value is a struct passed by value, so it is identified by the
 * address of the variable holding it. Its state is set from where it came
 * from -- the callee's *declared* return state, or the machine's initial
 * state for a value built directly -- never from an annotation, and every
 * use is checked against what the callee requires. Passing it to a
 * parameter that is not `read` (or returning it) consumes it.
 */

static TsVal *ts_find(Key key) {
    for (int i = 0; i < MAX_TSVALS; i++) {
        if (tsvals[i].in_use && tsvals[i].key == key) {
            return &tsvals[i];
        }
    }
    return NULL;
}

void phase_chk_ts_set(const void *v, const char *state, const char *name) {
    Key key = KEY(v);
    if (ts_find(key)) {
        fail("typestate value '%s' created over a live value", name);
    }
    for (int i = 0; i < MAX_TSVALS; i++) {
        if (!tsvals[i].in_use) {
            tsvals[i] = (TsVal){key, name, state, 0, 1};
            return;
        }
    }
    fail("too many live typestate values (limit %d)", MAX_TSVALS);
}

void phase_chk_ts_fresh(const void *v, const char *state, const char *initial, const char *name) {
    if (strcmp(state, initial) != 0) {
        fail("'%s' is created directly as %s, but a new value starts as %s", name, state, initial);
    }
    phase_chk_ts_set(v, state, name);
}

void phase_chk_ts_use(const void *v, const char *state, int consume, const char *user,
                      const char *name) {
    TsVal *t = ts_find(KEY(v));
    if (!t) {
        fail("'%s' is not a live typestate value (used by '%s')", name, user);
    }
    if (t->consumed) {
        fail("'%s' was already consumed; '%s' cannot use it", name, user);
    }
    if (strcmp(t->state, state) != 0) {
        fail("'%s' is %s, but '%s' requires %s", name, t->state, user, state);
    }
    if (consume) {
        t->consumed = 1;
    }
}

void phase_chk_ts_end(const void *v, const char *name) {
    TsVal *t = ts_find(KEY(v));
    if (!t) {
        fail("typestate value '%s' ends its scope but was never created", name);
    }
    t->in_use = 0;
}
