//! Random PHASE program generator.
//!
//! Programs are always syntactically valid and only use declared names
//! (otherwise nearly everything would be rejected for boring reasons). Each
//! operation is chosen against a rough model of every buffer's type, domain
//! and borrows, and of every typestate value's state, and is *usually*
//! consistent with it -- so a useful fraction of programs pass `phase
//! check` -- but a small, tunable fraction of choices ignore the model, and
//! blocks sometimes skip the "repair" that would put things back. Those are
//! the programs that probe the analyzer's edges.
//!
//! Buffers come in three types (`u8` x 8, `u8` x 16, `f32` x 8) and meet
//! externs and helpers with fixed and `_` lengths; typestate values follow
//! a `Pkt { Raw -> Parsed -> Checked }` machine.

use std::fmt::Write as _;

/// xorshift64*: tiny, deterministic, good enough for test generation.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() % 10_000) as f64 / 10_000.0 < p
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            Some(&items[self.below(items.len())])
        }
    }
}

/// Knobs for how adventurous generated programs are.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Probability that an operation ignores the model.
    pub chaos: f64,
    /// Probability that a block skips repairing its buffers on exit.
    pub skip_repair: f64,
    /// Maximum `if`/`while` nesting depth.
    pub max_depth: usize,
    /// Statements per block, at most.
    pub max_stmts: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            chaos: 0.08,
            skip_repair: 0.12,
            max_depth: 3,
            max_stmts: 7,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Dom {
    Ram,
    Dma,
    Device,
}

impl Dom {
    fn name(self) -> &'static str {
        match self {
            Dom::Ram => "RAM",
            Dom::Dma => "DMA",
            Dom::Device => "DEVICE",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Elem {
    U8,
    F32,
}

impl Elem {
    fn name(self) -> &'static str {
        match self {
            Elem::U8 => "u8",
            Elem::F32 => "f32",
        }
    }
}

/// A buffer's type; `len: None` is a `_` parameter (run-time length).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ty {
    elem: Elem,
    len: Option<u32>,
}

impl Ty {
    const fn new(elem: Elem, len: u32) -> Self {
        Ty {
            elem,
            len: Some(len),
        }
    }

    fn text(self) -> String {
        match self.len {
            Some(n) => format!("buffer<{}, {n}>", self.elem.name()),
            None => format!("buffer<{}, _>", self.elem.name()),
        }
    }

    /// Can a buffer of this type be passed where `want` is required?
    fn fits(self, want: Ty) -> bool {
        self.elem == want.elem && (want.len.is_none() || self.len == want.len)
    }
}

/// The types declared buffers are given.
const DECL_TYPES: &[Ty] = &[
    Ty::new(Elem::U8, 8),
    Ty::new(Elem::U8, 8),
    Ty::new(Elem::U8, 16),
    Ty::new(Elem::F32, 8),
];

#[derive(Clone, Debug)]
struct Buf {
    name: String,
    ty: Ty,
    dom: Dom,
    destroyed: bool,
    /// For a parameter: the domain it must be in at return.
    exit_dom: Option<Dom>,
    read_only: bool,
}

#[derive(Clone, Debug)]
struct Handle {
    name: String,
    target: String,
    ty: Ty,
    write: bool,
    released: bool,
}

/// A typestate value: 0 = Raw, 1 = Parsed, 2 = Checked.
#[derive(Clone, Debug)]
struct Pkt {
    name: String,
    state: usize,
    consumed: bool,
}

const PKT_STATES: [&str; 3] = ["Raw", "Parsed", "Checked"];

/// One parameter shape a generated helper can have.
#[derive(Clone, Copy, Debug)]
struct ParamKind {
    text: &'static str,
    ty: Ty,
    entry: Dom,
    exit: Dom,
    read_only: bool,
}

const fn pk(text: &'static str, ty: Ty, entry: Dom, exit: Dom, read_only: bool) -> ParamKind {
    ParamKind {
        text,
        ty,
        entry,
        exit,
        read_only,
    }
}

const U8_8: Ty = Ty::new(Elem::U8, 8);
const U8_ANY: Ty = Ty {
    elem: Elem::U8,
    len: None,
};
const F32_8: Ty = Ty::new(Elem::F32, 8);
const F32_ANY: Ty = Ty {
    elem: Elem::F32,
    len: None,
};

const PARAM_KINDS: &[ParamKind] = &[
    pk("buffer<u8, 8> @RAM", U8_8, Dom::Ram, Dom::Ram, false),
    pk("read buffer<u8, 8> @RAM", U8_8, Dom::Ram, Dom::Ram, true),
    pk(
        "buffer<u8, 8> @DMA -> @RAM",
        U8_8,
        Dom::Dma,
        Dom::Ram,
        false,
    ),
    pk(
        "write buffer<u8, 8> @RAM -> @DMA",
        U8_8,
        Dom::Ram,
        Dom::Dma,
        false,
    ),
    pk("buffer<u8, 8> @DMA", U8_8, Dom::Dma, Dom::Dma, false),
    pk("buffer<u8, _> @RAM", U8_ANY, Dom::Ram, Dom::Ram, false),
    pk("read buffer<u8, _> @RAM", U8_ANY, Dom::Ram, Dom::Ram, true),
    pk(
        "write buffer<u8, _> @RAM -> @DMA",
        U8_ANY,
        Dom::Ram,
        Dom::Dma,
        false,
    ),
    pk(
        "buffer<f32, _> @DMA -> @RAM",
        F32_ANY,
        Dom::Dma,
        Dom::Ram,
        false,
    ),
    pk("read buffer<f32, 8> @RAM", F32_8, Dom::Ram, Dom::Ram, true),
];

/// What an extern does to a buffer argument.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpKind {
    Look,
    Poke,
    Mix,
    Cmp,
    Start,
    Finish,
    Emit,
    Kick,
}

#[derive(Clone, Copy, Debug)]
struct ExternOp {
    name: &'static str,
    ty: Ty,
    kind: OpKind,
}

const fn eo(name: &'static str, ty: Ty, kind: OpKind) -> ExternOp {
    ExternOp { name, ty, kind }
}

const EXTERN_OPS: &[ExternOp] = &[
    eo("look", U8_8, OpKind::Look),
    eo("poke", U8_8, OpKind::Poke),
    eo("mix", U8_8, OpKind::Mix),
    eo("cmp", U8_8, OpKind::Cmp),
    eo("start", U8_8, OpKind::Start),
    eo("finish", U8_8, OpKind::Finish),
    eo("emit", U8_8, OpKind::Emit),
    eo("kick", U8_8, OpKind::Kick),
    eo("glook", U8_ANY, OpKind::Look),
    eo("gpoke", U8_ANY, OpKind::Poke),
    eo("gmix", U8_ANY, OpKind::Mix),
    eo("gstart", U8_ANY, OpKind::Start),
    eo("gfinish", U8_ANY, OpKind::Finish),
    eo("gemit", U8_ANY, OpKind::Emit),
    eo("flook", F32_ANY, OpKind::Look),
    eo("flook8", F32_8, OpKind::Look),
    eo("fpoke", F32_ANY, OpKind::Poke),
    eo("fmix", F32_ANY, OpKind::Mix),
    eo("fstart", F32_ANY, OpKind::Start),
    eo("ffinish", F32_ANY, OpKind::Finish),
    eo("femit", F32_ANY, OpKind::Emit),
];

#[derive(Clone, Debug)]
struct Helper {
    name: String,
    params: Vec<ParamKind>,
}

/// The fixed vocabulary every generated program declares: externs for
/// each buffer type (fixed and `_` lengths), and a typestate machine with
/// its externs and three PHASE helpers.
pub const EXTERNS: &str = "\
extern fn flip() -> bool;
extern fn count() -> u32;
extern fn look(x: read buffer<u8, 8> @RAM);
extern fn poke(x: write buffer<u8, 8> @RAM);
extern fn mix(a: read buffer<u8, 8> @RAM, b: write buffer<u8, 8> @RAM);
extern fn cmp(a: read buffer<u8, 8> @RAM, b: read buffer<u8, 8> @RAM);
extern fn start(x: write buffer<u8, 8> @RAM -> @DMA);
extern fn finish(x: buffer<u8, 8> @DMA -> @RAM);
extern fn emit(x: write buffer<u8, 8> @RAM -> @DEVICE);
extern fn kick(x: buffer<u8, 8> @DMA);
extern fn glook(x: read buffer<u8, _> @RAM);
extern fn gpoke(x: write buffer<u8, _> @RAM);
extern fn gmix(a: read buffer<u8, _> @RAM, b: write buffer<u8, _> @RAM);
extern fn gstart(x: write buffer<u8, _> @RAM -> @DMA);
extern fn gfinish(x: buffer<u8, _> @DMA -> @RAM);
extern fn gemit(x: write buffer<u8, _> @RAM -> @DEVICE);
extern fn flook(x: read buffer<f32, _> @RAM);
extern fn flook8(x: read buffer<f32, 8> @RAM);
extern fn fpoke(x: write buffer<f32, _> @RAM);
extern fn fmix(a: read buffer<f32, _> @RAM, b: write buffer<f32, _> @RAM);
extern fn fstart(x: write buffer<f32, _> @RAM -> @DMA);
extern fn ffinish(x: buffer<f32, _> @DMA -> @RAM);
extern fn femit(x: write buffer<f32, _> @RAM -> @DEVICE);

entity Pkt : @RAM { seq: u32 }
state Pkt { Raw -> Parsed -> Checked }
extern fn recv() -> Pkt<Raw>;
extern fn parse(p: Pkt<Raw>) -> Pkt<Parsed>;
extern fn verify(p: Pkt<Parsed>) -> Pkt<Checked>;
extern fn peekraw(p: read Pkt<Raw>);
extern fn peek(p: read Pkt<Checked>);
extern fn deliver(p: Pkt<Checked>);
extern fn drop_raw(p: Pkt<Raw>);

fn tadmit() -> Pkt<Checked> {
    let a: Pkt<Raw> = recv();
    let b: Pkt<Parsed> = parse(a);
    let c: Pkt<Checked> = verify(b);
    return c;
}

fn tparse(p: Pkt<Raw>) -> Pkt<Parsed> {
    let q: Pkt<Parsed> = parse(p);
    return q;
}

fn tshow(p: read Pkt<Checked>) {
    peek(p);
}
";

struct FnGen<'a> {
    rng: &'a mut Rng,
    cfg: Config,
    helpers: &'a [Helper],
    out: String,
    counter: usize,
    bufs: Vec<Buf>,
    handles: Vec<Handle>,
    pkts: Vec<Pkt>,
    /// Typestate values below this index were created outside the
    /// innermost loop: consuming one there would reuse it next iteration.
    pkt_floor: usize,
}

impl FnGen<'_> {
    fn fresh(&mut self, prefix: &str) -> String {
        self.counter += 1;
        format!("{prefix}{}", self.counter)
    }

    fn line(&mut self, depth: usize, text: &str) {
        let _ = writeln!(self.out, "{}{text}", "    ".repeat(depth + 1));
    }

    fn chaos(&mut self) -> bool {
        self.rng.chance(self.cfg.chaos)
    }

    /// Live (not destroyed) buffers, optionally filtered by domain and
    /// by whether they are free of borrows.
    fn candidates(&self, dom: Option<Dom>, unborrowed: bool, writable: bool) -> Vec<String> {
        self.bufs
            .iter()
            .filter(|b| !b.destroyed)
            .filter(|b| dom.is_none_or(|d| b.dom == d))
            .filter(|b| !unborrowed || !self.borrowed(&b.name))
            .filter(|b| !writable || !b.read_only)
            .map(|b| b.name.clone())
            .collect()
    }

    fn ty_of(&self, name: &str) -> Option<Ty> {
        self.bufs
            .iter()
            .find(|b| b.name == name)
            .map(|b| b.ty)
            .or_else(|| self.handles.iter().find(|h| h.name == name).map(|h| h.ty))
    }

    fn borrowed(&self, name: &str) -> bool {
        self.handles.iter().any(|h| h.target == name && !h.released)
    }

    fn write_borrowed(&self, name: &str) -> bool {
        self.handles
            .iter()
            .any(|h| h.target == name && !h.released && h.write)
    }

    fn any_buf(&mut self) -> Option<String> {
        let all: Vec<String> = self.bufs.iter().map(|b| b.name.clone()).collect();
        self.rng.pick(&all).cloned()
    }

    fn buf_mut(&mut self, name: &str) -> Option<&mut Buf> {
        self.bufs.iter_mut().find(|b| b.name == name)
    }

    /// A buffer usable for a parameter needing `dom`, with `write` access
    /// (or read-only access when `write` is false). May return a borrow
    /// handle instead of an owner.
    fn arg_for(&mut self, ty: Ty, dom: Dom, write: bool, handoff: bool) -> Option<String> {
        if self.chaos() {
            return self.any_buf();
        }
        let mut options: Vec<String> = self
            .bufs
            .iter()
            .filter(|b| !b.destroyed && b.dom == dom && b.ty.fits(ty))
            .filter(|b| !(write || handoff) || !b.read_only)
            .filter(|b| {
                if write || handoff {
                    !self.borrowed(&b.name)
                } else {
                    !self.write_borrowed(&b.name)
                }
            })
            .map(|b| b.name.clone())
            .collect();
        if dom == Dom::Ram && !handoff {
            for h in self.handles.iter().filter(|h| !h.released && h.ty.fits(ty)) {
                if h.write || !write {
                    options.push(h.name.clone());
                }
            }
        }
        self.rng.pick(&options).cloned()
    }

    fn set_dom(&mut self, name: &str, dom: Dom) {
        if let Some(b) = self.buf_mut(name) {
            b.dom = dom;
        }
    }

    // ---- statements ----------------------------------------------------

    /// Returns true if the block ended in a `return`.
    fn block(&mut self, depth: usize, in_loop: bool) -> bool {
        let n = 1 + self.rng.below(self.cfg.max_stmts);
        for _ in 0..n {
            if self.stmt(depth, in_loop) {
                return true; // emitted a `return`: nothing may follow it
            }
        }
        false
    }

    /// Emit one statement. Returns true if it was a `return`.
    fn stmt(&mut self, depth: usize, in_loop: bool) -> bool {
        match self.rng.below(130) {
            0..=11 => self.declare(depth),
            12..=19 => self.op_move(depth),
            20..=27 => self.op_sync(depth),
            28..=49 => self.op_call(depth),
            50..=58 => self.op_borrow(depth),
            59..=64 => self.op_release(depth),
            65..=67 => self.op_destroy(depth),
            68..=72 => self.op_swap(depth),
            73..=80 if depth < self.cfg.max_depth => self.op_if(depth, in_loop),
            81..=86 if depth < self.cfg.max_depth => self.op_while(depth),
            87..=91 => self.op_helper_call(depth),
            92..=93 if depth > 0 && !in_loop => {
                self.repair_all(depth);
                self.line(depth, "return;");
                return true;
            }
            94..=101 => self.ts_create(depth),
            102..=111 => self.ts_advance(depth),
            112..=118 => self.ts_inspect(depth),
            119..=125 => self.ts_finish(depth),
            _ => self.op_call(depth),
        }
        false
    }

    fn declare(&mut self, depth: usize) {
        let name = self.fresh("b");
        let dom = if self.rng.chance(0.2) {
            Dom::Dma
        } else {
            Dom::Ram
        };
        let ty = DECL_TYPES[self.rng.below(DECL_TYPES.len())];
        self.line(depth, &format!("{} {name} @{};", ty.text(), dom.name()));
        self.bufs.push(Buf {
            name,
            ty,
            dom,
            destroyed: false,
            exit_dom: None,
            read_only: false,
        });
    }

    fn op_move(&mut self, depth: usize) {
        let name = if self.chaos() {
            self.any_buf()
        } else {
            let c = self.candidates(Some(Dom::Ram), true, true);
            self.rng.pick(&c).cloned()
        };
        let Some(name) = name else { return };
        let to = if self.rng.chance(0.6) {
            Dom::Dma
        } else {
            Dom::Device
        };
        self.line(depth, &format!("move {name} -> @{};", to.name()));
        self.set_dom(&name, to);
    }

    fn op_sync(&mut self, depth: usize) {
        let name = if self.chaos() {
            self.any_buf()
        } else {
            let mut c = self.candidates(Some(Dom::Dma), true, true);
            c.extend(self.candidates(Some(Dom::Device), true, true));
            self.rng.pick(&c).cloned()
        };
        let Some(name) = name else { return };
        self.line(depth, &format!("sync({name});"));
        self.set_dom(&name, Dom::Ram);
    }

    fn op_call(&mut self, depth: usize) {
        let op = EXTERN_OPS[self.rng.below(EXTERN_OPS.len())];
        let (name, ty) = (op.name, op.ty);
        match op.kind {
            OpKind::Look => {
                if let Some(x) = self.arg_for(ty, Dom::Ram, false, false) {
                    self.line(depth, &format!("{name}({x});"));
                }
            }
            OpKind::Poke => {
                if let Some(x) = self.arg_for(ty, Dom::Ram, true, false) {
                    self.line(depth, &format!("{name}({x});"));
                }
            }
            OpKind::Mix => {
                let a = self.arg_for(ty, Dom::Ram, false, false);
                let b = self.arg_for(ty, Dom::Ram, true, false);
                if let (Some(a), Some(b)) = (a, b) {
                    self.line(depth, &format!("{name}({a}, {b});"));
                }
            }
            OpKind::Cmp => {
                let a = self.arg_for(ty, Dom::Ram, false, false);
                let b = self.arg_for(ty, Dom::Ram, false, false);
                if let (Some(a), Some(b)) = (a, b) {
                    self.line(depth, &format!("{name}({a}, {b});"));
                }
            }
            OpKind::Start => {
                if let Some(x) = self.arg_for(ty, Dom::Ram, true, true) {
                    self.line(depth, &format!("{name}({x});"));
                    self.set_dom(&x, Dom::Dma);
                }
            }
            OpKind::Finish => {
                if let Some(x) = self.arg_for(ty, Dom::Dma, false, true) {
                    self.line(depth, &format!("{name}({x});"));
                    self.set_dom(&x, Dom::Ram);
                }
            }
            OpKind::Kick => {
                if let Some(x) = self.arg_for(ty, Dom::Dma, false, true) {
                    self.line(depth, &format!("{name}({x});"));
                }
            }
            OpKind::Emit => {
                if let Some(x) = self.arg_for(ty, Dom::Ram, true, true) {
                    self.line(depth, &format!("{name}({x});"));
                    self.set_dom(&x, Dom::Device);
                }
            }
        }
    }

    /// The extern that hands a buffer of `ty` to `dom` (for repairs).
    fn handoff_extern(ty: Ty, dom: Dom) -> &'static str {
        match (ty.elem, dom) {
            (Elem::U8, Dom::Device) => "gemit",
            (Elem::U8, _) => "gstart",
            (Elem::F32, Dom::Device) => "femit",
            (Elem::F32, _) => "fstart",
        }
    }

    // ---- typestate values ----------------------------------------------

    fn live_pkts(&self, state: Option<usize>, consumable: bool) -> Vec<usize> {
        (0..self.pkts.len())
            .filter(|&i| !self.pkts[i].consumed)
            .filter(|&i| state.is_none_or(|s| self.pkts[i].state == s))
            .filter(|&i| !consumable || i >= self.pkt_floor)
            .collect()
    }

    /// A typestate value to use: usually one that fits. Under chaos, the
    /// deviations that matter: a value already consumed, or (when it will
    /// be consumed) one created outside the current loop.
    fn pick_pkt(&mut self, state: Option<usize>, consumable: bool) -> Option<usize> {
        // Typestate operations are a small share of statements, so their
        // deviations get a higher rate than the global chaos setting.
        let deviate = self.rng.chance((self.cfg.chaos * 3.0).min(0.5));
        if deviate && !self.pkts.is_empty() {
            let wrong: Vec<usize> = (0..self.pkts.len())
                .filter(|&i| self.pkts[i].consumed || (consumable && i < self.pkt_floor))
                .collect();
            return match self.rng.pick(&wrong) {
                Some(&i) => Some(i),
                None => Some(self.rng.below(self.pkts.len())),
            };
        }
        let c = self.live_pkts(state, consumable);
        self.rng.pick(&c).copied()
    }

    fn new_pkt(&mut self, depth: usize, state: usize, init: &str) {
        let name = self.fresh("p");
        // Under chaos, claim a state the initializer doesn't produce -- and
        // then use the value as if the claim were true, as a forger would.
        let claimed = if self.chaos() {
            self.rng.below(3)
        } else {
            state
        };
        self.line(
            depth,
            &format!("let {name}: Pkt<{}> = {init};", PKT_STATES[claimed]),
        );
        self.pkts.push(Pkt {
            name,
            state: claimed,
            consumed: false,
        });
    }

    fn ts_create(&mut self, depth: usize) {
        match self.rng.below(4) {
            0 => {
                let seq = self.rng.below(100);
                self.new_pkt(depth, 0, &format!("Pkt {{ seq: {seq} }}"));
            }
            1 => self.new_pkt(depth, 2, "tadmit()"),
            _ => self.new_pkt(depth, 0, "recv()"),
        }
    }

    fn ts_advance(&mut self, depth: usize) {
        let Some(i) = self.pick_pkt(None, true) else {
            return self.ts_create(depth);
        };
        let p = self.pkts[i].name.clone();
        let (init, next) = match self.pkts[i].state {
            0 if self.rng.chance(0.4) => (format!("tparse({p})"), 1),
            0 => (format!("parse({p})"), 1),
            1 => (format!("verify({p})"), 2),
            _ => return self.ts_finish(depth),
        };
        self.pkts[i].consumed = true;
        self.new_pkt(depth, next, &init);
    }

    fn ts_inspect(&mut self, depth: usize) {
        let Some(i) = self.pick_pkt(None, false) else {
            return;
        };
        let p = self.pkts[i].name.clone();
        let f = match self.pkts[i].state {
            0 => "peekraw",
            2 if self.rng.chance(0.4) => "tshow",
            _ => "peek",
        };
        if self.rng.chance(0.3) {
            let r = self.fresh("r");
            self.line(depth, &format!("let {r} = borrow {p} read;"));
            self.line(depth, &format!("{f}({r});"));
            self.line(depth, &format!("release {r};"));
        } else {
            self.line(depth, &format!("{f}({p});"));
        }
    }

    fn ts_finish(&mut self, depth: usize) {
        let Some(i) = self.pick_pkt(None, true) else {
            return self.ts_create(depth);
        };
        let p = self.pkts[i].name.clone();
        let f = match self.pkts[i].state {
            0 => "drop_raw",
            2 => "deliver",
            _ => return,
        };
        self.line(depth, &format!("{f}({p});"));
        self.pkts[i].consumed = true;
    }

    fn op_borrow(&mut self, depth: usize) {
        let write = self.rng.chance(0.35);
        let target = if self.chaos() {
            self.any_buf()
        } else {
            let c: Vec<String> = self
                .candidates(Some(Dom::Ram), false, write)
                .into_iter()
                .filter(|n| !self.write_borrowed(n) && (!write || !self.borrowed(n)))
                .collect();
            self.rng.pick(&c).cloned()
        };
        let Some(target) = target else { return };
        let Some(ty) = self.ty_of(&target) else {
            return;
        };
        let name = self.fresh("h");
        let mode = if write { "write" } else { "read" };
        self.line(depth, &format!("let {name} = borrow {target} {mode};"));
        self.handles.push(Handle {
            name,
            target,
            ty,
            write,
            released: false,
        });
    }

    fn op_release(&mut self, depth: usize) {
        let live: Vec<usize> = (0..self.handles.len())
            .filter(|&i| !self.handles[i].released || self.chaos())
            .collect();
        let Some(&i) = self.rng.pick(&live) else {
            return;
        };
        let name = self.handles[i].name.clone();
        self.line(depth, &format!("release {name};"));
        self.handles[i].released = true;
    }

    fn op_destroy(&mut self, depth: usize) {
        let name = if self.chaos() {
            self.any_buf()
        } else {
            let c: Vec<String> = self
                .candidates(Some(Dom::Ram), true, true)
                .into_iter()
                .filter(|n| {
                    self.bufs
                        .iter()
                        .any(|b| &b.name == n && b.exit_dom.is_none())
                })
                .collect();
            self.rng.pick(&c).cloned()
        };
        let Some(name) = name else { return };
        self.line(depth, &format!("destroy {name};"));
        if let Some(b) = self.buf_mut(&name) {
            b.destroyed = true;
        }
    }

    fn op_swap(&mut self, depth: usize) {
        let ignore_borrows = self.chaos();
        let c: Vec<String> = self
            .bufs
            .iter()
            .filter(|b| b.exit_dom.is_none() && !b.destroyed && !b.read_only)
            .map(|b| b.name.clone())
            .filter(|n| ignore_borrows || !self.borrowed(n))
            .collect();
        if c.len() < 2 {
            return;
        }
        let a = c[self.rng.below(c.len())].clone();
        let ta = self.ty_of(&a);
        let same: Vec<String> = c
            .iter()
            .filter(|n| **n != a && (self.chaos() || self.ty_of(n) == ta))
            .cloned()
            .collect();
        let Some(b) = self.rng.pick(&same).cloned() else {
            return;
        };
        self.line(depth, &format!("swap {a}, {b};"));
        let ia = self.bufs.iter().position(|x| x.name == a).unwrap();
        let ib = self.bufs.iter().position(|x| x.name == b).unwrap();
        let (da, db) = (self.bufs[ia].dom, self.bufs[ib].dom);
        self.bufs[ia].dom = db;
        self.bufs[ib].dom = da;
    }

    fn op_helper_call(&mut self, depth: usize) {
        let Some(h) = self.rng.pick(self.helpers).cloned() else {
            self.op_call(depth);
            return;
        };
        let mut args = Vec::new();
        for p in &h.params {
            let handoff = p.entry != p.exit || !p.read_only;
            let Some(a) = self.arg_for(p.ty, p.entry, !p.read_only, handoff) else {
                return;
            };
            args.push((a, p.exit));
        }
        let names: Vec<&str> = args.iter().map(|(a, _)| a.as_str()).collect();
        self.line(depth, &format!("{}({});", h.name, names.join(", ")));
        for (a, exit) in args {
            self.set_dom(&a, exit);
        }
    }

    fn op_if(&mut self, depth: usize, in_loop: bool) {
        let c = self.fresh("c");
        self.line(depth, &format!("let {c}: bool = flip();"));
        self.line(depth, &format!("if {c} {{"));
        let saved = (self.bufs.clone(), self.handles.clone(), self.pkts.clone());
        self.scoped_block(depth + 1, in_loop);
        let then_state = (self.bufs.clone(), self.handles.clone(), self.pkts.clone());
        (self.bufs, self.handles, self.pkts) = saved;
        if self.rng.chance(0.6) {
            self.line(depth, "} else {");
            self.scoped_block(depth + 1, in_loop);
        }
        self.line(depth, "}");
        // Continue from one of the two branches' views (they should agree
        // if the program is going to be accepted).
        if self.rng.chance(0.5) {
            (self.bufs, self.handles, self.pkts) = then_state;
        }
    }

    fn op_while(&mut self, depth: usize) {
        let n = self.fresh("n");
        let lim = self.fresh("l");
        self.line(depth, &format!("let {n}: u32 = 0;"));
        self.line(depth, &format!("let {lim}: u32 = count();"));
        self.line(depth, &format!("while {n} < {lim} {{"));
        let entry = self.bufs.clone();
        let floor = std::mem::replace(&mut self.pkt_floor, self.pkts.len());
        // (no `return` inside loop bodies, so the body always falls through)
        self.scoped_block(depth + 1, true);
        self.pkt_floor = floor;
        // Restore the loop invariant (usually).
        if !self.rng.chance(self.cfg.skip_repair) {
            self.restore(&entry, depth + 1);
        }
        self.line(depth + 1, &format!("{n} = {n} + 1;"));
        self.line(depth, "}");
    }

    /// A nested block: names declared inside go out of scope at its end.
    fn scoped_block(&mut self, depth: usize, in_loop: bool) {
        let nb = self.bufs.len();
        let nh = self.handles.len();
        let np = self.pkts.len();
        let returned = self.block(depth, in_loop);
        if !returned && !self.rng.chance(self.cfg.skip_repair) {
            self.repair_locals(nb, nh, depth);
        }
        self.bufs.truncate(nb);
        self.handles.truncate(nh);
        self.pkts.truncate(np);
    }

    /// Release this block's borrows and hand its buffers back to the CPU.
    fn repair_locals(&mut self, nb: usize, nh: usize, depth: usize) {
        for i in nh..self.handles.len() {
            if !self.handles[i].released {
                let h = self.handles[i].name.clone();
                self.line(depth, &format!("release {h};"));
                self.handles[i].released = true;
            }
        }
        for i in nb..self.bufs.len() {
            let b = &self.bufs[i];
            if !b.destroyed && b.dom != Dom::Ram {
                let name = b.name.clone();
                self.line(depth, &format!("sync({name});"));
                self.bufs[i].dom = Dom::Ram;
            }
        }
    }

    /// Bring every pre-existing buffer back to the domain it had in `entry`.
    fn restore(&mut self, entry: &[Buf], depth: usize) {
        for e in entry {
            let Some(i) = self.bufs.iter().position(|b| b.name == e.name) else {
                continue;
            };
            if self.bufs[i].destroyed || self.bufs[i].dom == e.dom {
                continue;
            }
            let name = e.name.clone();
            if self.bufs[i].dom != Dom::Ram {
                self.line(depth, &format!("sync({name});"));
                self.bufs[i].dom = Dom::Ram;
            }
            if e.dom != Dom::Ram {
                let f = Self::handoff_extern(self.bufs[i].ty, e.dom);
                self.line(depth, &format!("{f}({name});"));
                self.bufs[i].dom = e.dom;
            }
        }
    }

    /// Before a `return`: settle everything the function owns.
    fn repair_all(&mut self, depth: usize) {
        if self.rng.chance(self.cfg.skip_repair) {
            return;
        }
        for i in 0..self.handles.len() {
            if !self.handles[i].released {
                let h = self.handles[i].name.clone();
                self.line(depth, &format!("release {h};"));
                self.handles[i].released = true;
            }
        }
        for i in 0..self.bufs.len() {
            let b = self.bufs[i].clone();
            if b.destroyed {
                continue;
            }
            let want = b.exit_dom.unwrap_or(Dom::Ram);
            if b.dom == want {
                continue;
            }
            if b.dom != Dom::Ram {
                self.line(depth, &format!("sync({});", b.name));
                self.bufs[i].dom = Dom::Ram;
            }
            if want == Dom::Dma {
                let f = Self::handoff_extern(b.ty, Dom::Dma);
                self.line(depth, &format!("{f}({});", b.name));
                self.bufs[i].dom = Dom::Dma;
            }
        }
    }
}

/// Generate one complete program from `seed`.
pub fn program(seed: u64, cfg: Config) -> String {
    let mut rng = Rng::new(seed);
    let mut src = String::from(EXTERNS);

    let helpers: Vec<Helper> = (0..rng.below(3))
        .map(|i| Helper {
            name: format!("helper{i}"),
            params: (0..1 + rng.below(2))
                .map(|_| PARAM_KINDS[rng.below(PARAM_KINDS.len())])
                .collect(),
        })
        .collect();

    for h in &helpers {
        let mut g = FnGen {
            rng: &mut rng,
            cfg,
            helpers: &[],
            out: String::new(),
            counter: 0,
            bufs: Vec::new(),
            handles: Vec::new(),
            pkts: Vec::new(),
            pkt_floor: 0,
        };
        let mut params = Vec::new();
        for (i, p) in h.params.iter().enumerate() {
            let name = format!("p{i}");
            params.push(format!("{name}: {}", p.text));
            g.bufs.push(Buf {
                name,
                ty: p.ty,
                dom: p.entry,
                destroyed: false,
                exit_dom: Some(p.exit),
                read_only: p.read_only,
            });
        }
        if !g.block(0, false) {
            g.repair_all(0);
        }
        let _ = writeln!(
            src,
            "\nfn {}({}) {{\n{}}}",
            h.name,
            params.join(", "),
            g.out
        );
    }

    let mut g = FnGen {
        rng: &mut rng,
        cfg,
        helpers: &helpers,
        out: String::new(),
        counter: 0,
        bufs: Vec::new(),
        handles: Vec::new(),
        pkts: Vec::new(),
        pkt_floor: 0,
    };
    if !g.block(0, false) {
        g.repair_all(0);
    }
    let _ = writeln!(src, "\nfn main() {{\n{}}}", g.out);
    src
}
