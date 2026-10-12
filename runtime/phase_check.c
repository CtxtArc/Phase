/* Runtime ownership checking for `phase build --checked`. See
 * phase_check.h for what this is and why it is independent of the static
 * analyzer.
 */

#include "phase_check.h"

#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>

#define MAX_STORAGES 1024
#define MAX_HANDLES 4096

typedef struct {
    const void *p;
    const char *name;
    int domain;
    int readers;
    int writer;
    int destroyed;
    int in_use;
} Storage;

typedef struct {
    const void *p;
    const char *name;
    int write;
    int released;
    int in_use;
} Handle;

static Storage storages[MAX_STORAGES];
static Handle handles[MAX_HANDLES];

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

static Storage *find(const void *p, const char *name) {
    for (int i = 0; i < MAX_STORAGES; i++) {
        if (storages[i].in_use && storages[i].p == p) {
            return &storages[i];
        }
    }
    fail("'%s' does not refer to a live buffer", name);
    return NULL;
}

static Storage *live(const void *p, const char *name, const char *what) {
    Storage *s = find(p, name);
    if (s->destroyed) {
        fail("'%s' was destroyed; cannot %s", name, what);
    }
    return s;
}

static void no_borrows(const Storage *s, const char *name, const char *what) {
    if (s->readers || s->writer) {
        fail("'%s' has a live borrow; cannot %s", name, what);
    }
}

void phase_chk_create(const void *storage, int domain, const char *name) {
    for (int i = 0; i < MAX_STORAGES; i++) {
        if (storages[i].in_use && storages[i].p == storage) {
            fail("buffer '%s' created over live storage of '%s'", name, storages[i].name);
        }
    }
    for (int i = 0; i < MAX_STORAGES; i++) {
        if (!storages[i].in_use) {
            storages[i] = (Storage){storage, name, domain, 0, 0, 0, 1};
            return;
        }
    }
    fail("too many live buffers (limit %d)", MAX_STORAGES);
}

void phase_chk_scope_end(const void *storage, const char *name) {
    Storage *s = find(storage, name);
    if (!s->destroyed && device_owned(s->domain)) {
        fail("'%s' goes out of scope while owned by %s", name, dom_name(s->domain));
    }
    if (s->readers || s->writer) {
        fail("'%s' goes out of scope with a live borrow", name);
    }
    s->in_use = 0;
}

void phase_chk_move(const void *p, int to, const char *name) {
    Storage *s = live(p, name, "move it");
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
    Storage *s = live(p, name, "sync it");
    no_borrows(s, name, "sync it");
    if (!device_owned(s->domain)) {
        fail("'%s': sync from %s, which no hardware owns", name, dom_name(s->domain));
    }
    s->domain = PHASE_DOM_RAM;
}

void phase_chk_destroy(const void *p, const char *name) {
    Storage *s = live(p, name, "destroy it");
    no_borrows(s, name, "destroy it");
    if (device_owned(s->domain)) {
        fail("'%s' destroyed while owned by %s", name, dom_name(s->domain));
    }
    s->destroyed = 1;
}

void phase_chk_swap(const void *a, const void *b, const char *a_name, const char *b_name) {
    Storage *sa = live(a, a_name, "swap it");
    Storage *sb = live(b, b_name, "swap it");
    no_borrows(sa, a_name, "swap it");
    no_borrows(sb, b_name, "swap it");
}

int phase_chk_borrow(const void *p, int write, const char *name) {
    Storage *s = live(p, name, "borrow it");
    if (device_owned(s->domain)) {
        fail("borrow '%s' of a buffer owned by %s", name, dom_name(s->domain));
    }
    if (s->writer || (write && s->readers)) {
        fail("borrow '%s' conflicts with a live borrow of the same buffer", name);
    }
    if (write) {
        s->writer = 1;
    } else {
        s->readers++;
    }
    for (int i = 0; i < MAX_HANDLES; i++) {
        if (!handles[i].in_use) {
            handles[i] = (Handle){p, name, write, 0, 1};
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
    Storage *s = find(hd->p, name);
    if (hd->write) {
        s->writer = 0;
    } else {
        s->readers--;
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

void phase_chk_arg(const void *p, int via, int param_domain, int access,
                   const char *callee, const char *name) {
    Storage *s = live(p, name, "pass it to a function");
    if (param_domain >= 0 && s->domain != param_domain) {
        fail("'%s' is in %s but '%s' expects %s", name, dom_name(s->domain), callee,
             dom_name(param_domain));
    }
    switch (via) {
    case PHASE_VIA_OWNER:
        if (s->writer || (access != PHASE_ACC_READ && s->readers)) {
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
            if (ptrs[i] == ptrs[j] && !(read_only[i] && read_only[j])) {
                fail("the same buffer is passed to '%s' twice, and not read-only both times",
                     callee);
            }
        }
    }
}

void phase_chk_transfer(const void *p, int to, const char *callee, const char *name) {
    Storage *s = live(p, name, "hand it to hardware");
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
    Storage *s = find(p, name);
    if (!s->destroyed && s->domain != domain) {
        fail("parameter '%s' leaves '%s' in %s, but its signature says %s", name, fn,
             dom_name(s->domain), dom_name(domain));
    }
}
