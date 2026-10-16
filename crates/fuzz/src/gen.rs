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
//!
//! Since v2.3 every program also has a device (`Fz`, four registers of
//! different access modes and widths, two with fields) and three events,
//! and most have interrupt handlers: they write registers and signal
//! events, and the code they interrupt reads and writes registers, waits
//! (and has the handlers hand it buffers with `wait E for b`), signals, and
//! runs blocks in `critical`.

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
    /// Block depth it was declared at (`swap`/`rotate` need one block).
    depth: usize,
    /// Split into these parts (and unusable whole until they are joined).
    split: Option<Vec<String>>,
    /// The slice of a split buffer this name's storage is: moves with the
    /// storage on `swap`/`rotate`.
    part_of: Option<(String, usize)>,
}

impl Buf {
    fn usable(&self) -> bool {
        !self.destroyed && self.split.is_none()
    }
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

device Fz @ 0x40000000 {
    CTRL: read write u32 @ 0x00 = 0 { EN: bit 0, MODE: bits 1..=3, IE: bit 4 }
    STAT: read u8 @ 0x04;
    LOAD: write u16 @ 0x08;
    ACK: read write u8 @ 0x0C { DONE: bit 0, ERR: bit 1 }
}

event ev0;
event ev1;
event ev2;
";

/// A register of `Fz`: name, readable, writable, width, fields.
struct Reg {
    name: &'static str,
    readable: bool,
    writable: bool,
    bits: u32,
    fields: &'static [(&'static str, u32)],
}

const REGS: &[Reg] = &[
    Reg {
        name: "CTRL",
        readable: true,
        writable: true,
        bits: 32,
        fields: &[("EN", 1), ("MODE", 3), ("IE", 1)],
    },
    Reg {
        name: "STAT",
        readable: true,
        writable: false,
        bits: 8,
        fields: &[],
    },
    Reg {
        name: "LOAD",
        readable: false,
        writable: true,
        bits: 16,
        fields: &[],
    },
    Reg {
        name: "ACK",
        readable: true,
        writable: true,
        bits: 8,
        fields: &[("DONE", 1), ("ERR", 1)],
    },
];

const EVENTS: [&str; 3] = ["ev0", "ev1", "ev2"];

/// What the program's interrupt handlers do, for the code they interrupt.
#[derive(Default)]
struct Hw {
    /// Events some handler signals (every time it runs).
    signalled: Vec<&'static str>,
    /// Registers some handler writes.
    handler_regs: Vec<&'static str>,
}

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
    hw: &'a Hw,
    /// Generating an interrupt handler's body.
    in_handler: bool,
    /// Inside `critical { }`.
    in_critical: bool,
    /// `wait E for b` so far in this function: (E, b).
    handovers: Vec<(&'static str, String)>,
    /// Registers this function writes.
    written: Vec<&'static str>,
    /// No register or interrupt deviations at all (a handler that isn't
    /// the one deviating).
    clean: bool,
}

impl<'a> FnGen<'a> {
    fn new(rng: &'a mut Rng, cfg: Config, helpers: &'a [Helper], hw: &'a Hw) -> Self {
        FnGen {
            rng,
            cfg,
            helpers,
            out: String::new(),
            counter: 0,
            bufs: Vec::new(),
            handles: Vec::new(),
            pkts: Vec::new(),
            pkt_floor: 0,
            hw,
            in_handler: false,
            in_critical: false,
            handovers: Vec::new(),
            written: Vec::new(),
            clean: false,
        }
    }
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
            .filter(|b| b.usable())
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
        // Using a split buffer whole is rare otherwise: a targeted deviation.
        let split: Vec<String> = self
            .bufs
            .iter()
            .filter(|b| !b.destroyed && b.split.is_some() && b.dom == dom && b.ty.fits(ty))
            .map(|b| b.name.clone())
            .collect();
        if !split.is_empty() && self.rng.chance(self.cfg.chaos) {
            return self.rng.pick(&split).cloned();
        }
        let mut options: Vec<String> = self
            .bufs
            .iter()
            .filter(|b| b.usable() && b.dom == dom && b.ty.fits(ty))
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
        match self.rng.below(200) {
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
            // (A `return` inside `critical` would leave interrupts off:
            // only as a deviation.)
            92..=93 if depth > 0 && !in_loop && (!self.in_critical || self.targeted()) => {
                self.repair_all(depth);
                self.line(depth, "return;");
                return true;
            }
            94..=101 => self.ts_create(depth),
            102..=111 => self.ts_advance(depth),
            112..=118 => self.ts_inspect(depth),
            119..=125 => self.ts_finish(depth),
            130..=135 => self.op_split(depth),
            136..=140 => self.op_join(depth),
            141..=145 => self.op_rotate(depth),
            146..=155 => self.op_reg_write(depth),
            156..=161 => self.op_reg_read(depth),
            162..=169 => self.op_wait(depth),
            170..=175 if depth < self.cfg.max_depth => return self.op_critical(depth, in_loop),
            176..=177 => self.op_signal(depth),
            178..=199 => self.op_wait_for(depth),
            _ => self.op_call(depth),
        }
        false
    }

    fn declare(&mut self, depth: usize) {
        let ty = DECL_TYPES[self.rng.below(DECL_TYPES.len())];
        self.declare_as(depth, ty);
    }

    fn declare_as(&mut self, depth: usize, ty: Ty) -> String {
        let name = self.fresh("b");
        let dom = if self.rng.chance(0.2) {
            Dom::Dma
        } else {
            Dom::Ram
        };
        self.line(depth, &format!("{} {name} @{};", ty.text(), dom.name()));
        self.bufs.push(Buf {
            name,
            ty,
            dom,
            destroyed: false,
            exit_dom: None,
            read_only: false,
            depth,
            split: None,
            part_of: None,
        });
        self.bufs.last().unwrap().name.clone()
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

    // ---- registers and interrupts ---------------------------------------

    /// Interrupt deviations are a small share of statements, so they get a
    /// higher rate than the global chaos setting (like typestate's, if less
    /// so: a program has several register accesses, each a chance to
    /// deviate).
    fn hw_chaos(&mut self) -> bool {
        !self.clean && self.rng.chance((self.cfg.chaos * 1.5).min(0.5))
    }

    /// The interrupt rules about *where* something happens (a field write
    /// outside `critical`, a `wait` or `return` inside it) only get a
    /// chance to deviate in the rare places they apply, so those get a
    /// higher rate still.
    fn targeted(&mut self) -> bool {
        !self.clean && self.rng.chance((self.cfg.chaos * 4.0).min(0.5))
    }

    fn op_reg_write(&mut self, depth: usize) {
        let wrong_reg = self.hw_chaos();
        let regs: Vec<usize> = (0..REGS.len())
            .filter(|&i| wrong_reg || REGS[i].writable)
            .collect();
        let r = &REGS[*self.rng.pick(&regs).unwrap()];
        let field = if !r.fields.is_empty() && self.rng.chance(0.65) {
            Some(r.fields[self.rng.below(r.fields.len())])
        } else {
            None
        };
        let width = field.map_or(r.bits, |(_, w)| w);
        let max = if width >= 64 {
            u64::MAX
        } else {
            (1u64 << width) - 1
        };
        let value = if self.hw_chaos() {
            max + 1 + self.rng.below(3) as u64
        } else {
            match self.rng.below(3) {
                0 => 0,
                1 => max,
                _ => self.next_below(max),
            }
        };
        let target = match field {
            Some((f, _)) => format!("Fz.{}.{f}", r.name),
            None => format!("Fz.{}", r.name),
        };
        let write = format!("volatile_write({target}, {value});");
        if !self.written.contains(&r.name) {
            self.written.push(r.name);
        }
        // A field write is read-modify-write: if a handler writes the same
        // register, the code it interrupts must do it in `critical`.
        let racy = field.is_some()
            && !self.in_handler
            && !self.in_critical
            && self.hw.handler_regs.contains(&r.name);
        if racy && !self.targeted() {
            self.line(depth, &format!("critical {{ {write} }}"));
        } else {
            self.line(depth, &write);
        }
    }

    fn next_below(&mut self, max: u64) -> u64 {
        self.rng.next_u64() % (max.min(1 << 20) + 1)
    }

    fn op_reg_read(&mut self, depth: usize) {
        let wrong_reg = self.hw_chaos();
        let regs: Vec<usize> = (0..REGS.len())
            .filter(|&i| wrong_reg || REGS[i].readable)
            .collect();
        let r = &REGS[*self.rng.pick(&regs).unwrap()];
        let target = if !r.fields.is_empty() && self.rng.chance(0.5) {
            format!(
                "Fz.{}.{}",
                r.name,
                r.fields[self.rng.below(r.fields.len())].0
            )
        } else {
            format!("Fz.{}", r.name)
        };
        let v = self.fresh("v");
        self.line(depth, &format!("let {v}: u32 = volatile_read({target});"));
    }

    /// An event a `wait` can rely on: one a handler signals -- or, as a
    /// deviation, any event.
    fn wait_event(&mut self) -> Option<&'static str> {
        if self.hw_chaos() {
            return Some(EVENTS[self.rng.below(EVENTS.len())]);
        }
        self.rng.pick(&self.hw.signalled).copied()
    }

    /// Whether a `wait` may go here: not in a handler, not in `critical`
    /// (both only as deviations).
    fn may_wait(&mut self) -> bool {
        !(self.in_handler || self.in_critical) || self.targeted()
    }

    fn op_wait(&mut self, depth: usize) {
        if !self.may_wait() {
            return;
        }
        if let Some(e) = self.wait_event() {
            self.line(depth, &format!("wait {e};"));
        }
    }

    fn op_signal(&mut self, depth: usize) {
        // Only events a handler signals anyway: a `signal` in thread code
        // that a `wait` depended on could be on a path not taken, and the
        // program would hang (which the analyzer doesn't promise against).
        if let Some(&e) = self.rng.pick(&self.hw.signalled) {
            self.line(depth, &format!("signal {e};"));
        }
    }

    /// `wait E for b`: the handler signalling E hands `b` back.
    fn op_wait_for(&mut self, depth: usize) {
        if !self.may_wait() {
            return;
        }
        let mut in_flight = self.candidates(Some(Dom::Dma), true, true);
        in_flight.extend(self.candidates(Some(Dom::Device), true, true));
        if in_flight.is_empty() && !self.hw.signalled.is_empty() {
            match self.start_transfer(depth) {
                Some(b) => in_flight.push(b),
                None => return,
            }
        }
        let deviation = if self.targeted() {
            self.wait_for_deviation(depth, &in_flight)
        } else {
            None
        };
        let (event, buf) = match deviation {
            Some(pair) => pair,
            None => {
                // An event already handing over a buffer that is in flight,
                // or a new event for one.
                let mapped: Vec<(&'static str, String)> = self
                    .handovers
                    .iter()
                    .filter(|(_, b)| in_flight.contains(b))
                    .cloned()
                    .collect();
                let free = self.free_events();
                if !mapped.is_empty() && (free.is_empty() || self.rng.chance(0.6)) {
                    self.rng.pick(&mapped).cloned().unwrap()
                } else {
                    let (Some(&e), Some(b)) = (self.rng.pick(&free), self.rng.pick(&in_flight))
                    else {
                        return;
                    };
                    (e, b.clone())
                }
            }
        };
        self.line(depth, &format!("wait {event} for {buf};"));
        if !self.handovers.iter().any(|(e, _)| *e == event) {
            self.handovers.push((event, buf.clone()));
        }
        self.set_dom(&buf, Dom::Ram);
    }

    /// Hand a CPU buffer to the DMA engine, to have one to wait for.
    fn start_transfer(&mut self, depth: usize) -> Option<String> {
        let free = self.candidates(Some(Dom::Ram), true, true);
        let b = self.rng.pick(&free)?.clone();
        let ty = self.ty_of(&b)?;
        let f = Self::handoff_extern(ty, Dom::Dma);
        self.line(depth, &format!("{f}({b});"));
        self.set_dom(&b, Dom::Dma);
        Some(b)
    }

    /// Signalled events not yet handing over a buffer in this function.
    fn free_events(&self) -> Vec<&'static str> {
        self.hw
            .signalled
            .iter()
            .copied()
            .filter(|e| !self.handovers.iter().any(|(m, _)| m == e))
            .collect()
    }

    /// A `wait E for b` that breaks exactly one rule, if one can be made
    /// here: an event that hands over another buffer in this function
    /// (with `b` in flight, so ownership alone is fine), or a buffer the
    /// hardware doesn't have (with `E` free to hand it over).
    fn wait_for_deviation(
        &mut self,
        depth: usize,
        in_flight: &[String],
    ) -> Option<(&'static str, String)> {
        if !self.handovers.is_empty() && self.rng.chance(0.8) {
            let mut in_flight = in_flight.to_vec();
            if !self.handovers.is_empty() && in_flight.len() < 2 {
                in_flight.extend(self.start_transfer(depth));
            }
            let pairs: Vec<(&'static str, String)> = self
                .handovers
                .iter()
                .flat_map(|(e, mapped)| {
                    in_flight
                        .iter()
                        .filter(move |b| *b != mapped)
                        .map(move |b| (*e, b.clone()))
                })
                .collect();
            self.rng.pick(&pairs).cloned()
        } else {
            let cpu = self.candidates(Some(Dom::Ram), true, true);
            let b = self.rng.pick(&cpu)?.clone();
            let mut events = self.free_events();
            events.extend(
                self.handovers
                    .iter()
                    .filter(|(_, m)| *m == b)
                    .map(|(e, _)| *e),
            );
            let e = *self.rng.pick(&events)?;
            Some((e, b))
        }
    }

    /// Returns true if the block returned (so nothing may follow it).
    fn op_critical(&mut self, depth: usize, in_loop: bool) -> bool {
        self.line(depth, "critical {");
        let was = std::mem::replace(&mut self.in_critical, true);
        let returned = self.scoped_block(depth + 1, in_loop);
        self.in_critical = was;
        self.line(depth, "}");
        returned
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
        let name = self.fresh("k");
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
                        .any(|b| &b.name == n && b.exit_dom.is_none() && b.part_of.is_none())
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

    /// Buffers `swap`/`rotate` could permute together with `first`.
    fn permutable(&self, ignore_borrows: bool) -> Vec<String> {
        self.bufs
            .iter()
            .filter(|b| b.exit_dom.is_none() && b.usable() && !b.read_only)
            .map(|b| b.name.clone())
            .filter(|n| ignore_borrows || !self.borrowed(n))
            .collect()
    }

    /// Up to `k` distinct names starting with a random one, all of its
    /// type and block (unless chaos says otherwise).
    fn permutation_group(&mut self, depth: usize, k: usize) -> Option<Vec<String>> {
        let ignore_borrows = self.chaos();
        let mut c = self.permutable(ignore_borrows);
        if c.is_empty() {
            let ty = DECL_TYPES[self.rng.below(DECL_TYPES.len())];
            c.push(self.declare_as(depth, ty));
        }
        // Usually start from a part of a split buffer, when there is one:
        // that is where permutations interact with `join`.
        let parts: Vec<String> = c
            .iter()
            .filter(|n| {
                self.bufs
                    .iter()
                    .any(|b| &b.name == *n && b.part_of.is_some())
            })
            .cloned()
            .collect();
        let a = if !parts.is_empty() && self.rng.chance(0.6) {
            parts[self.rng.below(parts.len())].clone()
        } else {
            c[self.rng.below(c.len())].clone()
        };
        let (ta, da) = {
            let b = self.bufs.iter().find(|b| b.name == a).unwrap();
            (b.ty, b.depth)
        };
        let mut same: Vec<String> = c
            .iter()
            .filter(|n| **n != a)
            .filter(|n| {
                self.chaos() || {
                    let b = self.bufs.iter().find(|b| &b.name == *n).unwrap();
                    b.ty == ta && b.depth == da
                }
            })
            .cloned()
            .collect();
        let mut group = vec![a];
        while group.len() < k && !same.is_empty() {
            group.push(same.remove(self.rng.below(same.len())));
        }
        // Too few partners: declare some (in the first one's block, which
        // is only possible when that is this block).
        while group.len() < k && da == depth {
            group.push(self.declare_as(depth, ta));
        }
        (group.len() >= 2).then_some(group)
    }

    /// Each name takes the next one's storage (the last takes the first's).
    fn permute(&mut self, names: &[String]) {
        let idx: Vec<usize> = names
            .iter()
            .map(|n| self.bufs.iter().position(|x| &x.name == n).unwrap())
            .collect();
        let moved: Vec<(Dom, Option<(String, usize)>)> = idx
            .iter()
            .map(|&i| (self.bufs[i].dom, self.bufs[i].part_of.clone()))
            .collect();
        for (k, &i) in idx.iter().enumerate() {
            let (dom, part_of) = moved[(k + 1) % idx.len()].clone();
            self.bufs[i].dom = dom;
            self.bufs[i].part_of = part_of;
        }
    }

    fn op_swap(&mut self, depth: usize) {
        let Some(g) = self.permutation_group(depth, 2) else {
            return;
        };
        self.line(depth, &format!("swap {};", g.join(", ")));
        self.permute(&g);
    }

    fn op_rotate(&mut self, depth: usize) {
        let k = 2 + self.rng.below(3);
        let Some(g) = self.permutation_group(depth, k) else {
            return;
        };
        self.line(depth, &format!("rotate {};", g.join(", ")));
        self.permute(&g);
    }

    fn op_split(&mut self, depth: usize) {
        let parent = if self.chaos() {
            self.any_buf()
        } else {
            let c: Vec<String> = self
                .candidates(None, true, true)
                .into_iter()
                .filter(|n| {
                    self.bufs
                        .iter()
                        .any(|b| &b.name == n && b.exit_dom.is_none())
                })
                .collect();
            self.rng.pick(&c).cloned().or_else(|| {
                let ty = DECL_TYPES[self.rng.below(DECL_TYPES.len())];
                Some(self.declare_as(depth, ty))
            })
        };
        let Some(parent) = parent else { return };
        let Some(i) = self.bufs.iter().position(|b| b.name == parent) else {
            return;
        };
        let (ty, dom) = (self.bufs[i].ty, self.bufs[i].dom);
        let Some(len) = ty.len else { return };
        let n = if self.chaos() {
            3
        } else if len % 4 == 0 && self.rng.chance(0.3) {
            4
        } else {
            2
        };
        let parts: Vec<String> = (0..n).map(|_| self.fresh("s")).collect();
        self.line(depth, &format!("split {parent} into {};", parts.join(", ")));
        if len % n as u32 != 0 {
            return; // rejected; the model does not matter any more
        }
        let part_ty = Ty::new(ty.elem, len / n as u32);
        self.bufs[i].split = Some(parts.clone());
        for (k, name) in parts.into_iter().enumerate() {
            self.bufs.push(Buf {
                name,
                ty: part_ty,
                dom,
                destroyed: false,
                exit_dom: None,
                read_only: false,
                depth,
                split: None,
                part_of: Some((parent.clone(), k)),
            });
        }
    }

    /// Split buffers, innermost first.
    fn split_parents(&self) -> Vec<usize> {
        (0..self.bufs.len())
            .rev()
            .filter(|&i| !self.bufs[i].destroyed && self.bufs[i].split.is_some())
            .collect()
    }

    /// Can `parent` be joined as things stand? (`strict` false: ignoring
    /// whether each slice is in place and the parts share a domain.)
    fn joinable(&self, parent: usize, strict: bool) -> bool {
        let pname = &self.bufs[parent].name;
        let parts = self.bufs[parent].split.as_ref().unwrap();
        let mut dom = None;
        for (k, name) in parts.iter().enumerate() {
            let Some(b) = self.bufs.iter().find(|b| &b.name == name) else {
                return false;
            };
            if !b.usable()
                || self.borrowed(name)
                || (strict && b.part_of.as_ref() != Some(&(pname.clone(), k)))
                || (strict && dom.is_some_and(|d| d != b.dom))
            {
                return false;
            }
            dom = Some(b.dom);
        }
        true
    }

    fn join(&mut self, depth: usize, parent: usize) {
        let pname = self.bufs[parent].name.clone();
        let parts = self.bufs[parent].split.clone().unwrap();
        self.line(depth, &format!("join {} into {pname};", parts.join(", ")));
        // (Parts in different domains are an error; the model carries on
        // with the first part's domain.)
        let mut dom = None;
        for name in &parts {
            if let Some(b) = self.buf_mut(name) {
                dom = dom.or(Some(b.dom));
                b.destroyed = true;
            }
        }
        let dom = dom.unwrap_or(self.bufs[parent].dom);
        self.bufs[parent].dom = dom;
        self.bufs[parent].split = None;
    }

    fn op_join(&mut self, depth: usize) {
        let parents = self.split_parents();
        let chaos = self.chaos();
        // Joining parts out of place or across domains is rare otherwise,
        // so that deviation gets a higher rate than the global chaos one.
        let strict = !self.rng.chance((self.cfg.chaos * 3.0).min(0.5));
        let c: Vec<usize> = parents
            .into_iter()
            .filter(|&i| chaos || self.joinable(i, strict))
            .collect();
        if let Some(&i) = self.rng.pick(&c) {
            if !strict && self.joinable(i, true) {
                self.unsettle_parts(depth, i);
            }
            self.join(depth, i);
        } else if let Some(&i) = self.rng.pick(&self.split_parents()) {
            // Not joinable yet: put its slices back in place first.
            if self
                .bufs
                .iter()
                .all(|b| b.destroyed || b.name == self.bufs[i].name || !self.borrowed(&b.name))
            {
                self.rejoin(depth, i);
            }
        }
    }

    /// A deviation just before a join: hand one part to the DMA engine, or
    /// swap two of them out of place.
    fn unsettle_parts(&mut self, depth: usize, parent: usize) {
        let parts = self.bufs[parent].split.clone().unwrap();
        let last = parts.last().unwrap().clone();
        let Some(b) = self.bufs.iter().find(|b| b.name == last) else {
            return;
        };
        if self.rng.chance(0.5) && b.dom == Dom::Ram {
            let f = Self::handoff_extern(b.ty, Dom::Dma);
            self.line(depth, &format!("{f}({last});"));
            self.set_dom(&last, Dom::Dma);
        } else {
            let pair = [parts[0].clone(), last];
            self.line(depth, &format!("swap {};", pair.join(", ")));
            self.permute(&pair);
        }
    }

    /// Put each slice of `parent` back under its own part's name, settle
    /// the parts in @RAM, and join them.
    fn rejoin(&mut self, depth: usize, parent: usize) {
        let pname = self.bufs[parent].name.clone();
        let parts = self.bufs[parent].split.clone().unwrap();
        for (k, name) in parts.iter().enumerate() {
            let slot = Some((pname.clone(), k));
            let Some(holder) = self.bufs.iter().find(|b| b.part_of == slot && !b.destroyed) else {
                return;
            };
            if &holder.name != name {
                let pair = [name.clone(), holder.name.clone()];
                self.line(depth, &format!("swap {};", pair.join(", ")));
                self.permute(&pair);
            }
        }
        for name in &parts {
            let Some(b) = self.bufs.iter().find(|b| &b.name == name) else {
                return;
            };
            if !b.destroyed && b.dom != Dom::Ram {
                self.line(depth, &format!("sync({name});"));
                self.set_dom(name, Dom::Ram);
            }
        }
        if self.joinable(parent, true) {
            if self.rng.chance((self.cfg.chaos * 3.0).min(0.5)) {
                self.unsettle_parts(depth, parent);
            }
            self.join(depth, parent);
        }
    }

    /// At the end of `main`: call every helper once more, with fresh
    /// buffers prepared for it, so its body runs (and is checked at run
    /// time) even when no random call reached it.
    fn call_every_helper(&mut self, depth: usize) {
        let helpers = self.helpers.to_vec();
        for h in &helpers {
            let mut args = Vec::new();
            for p in &h.params {
                let ty = Ty::new(p.ty.elem, p.ty.len.unwrap_or(8));
                let b = self.fresh("b");
                self.line(depth, &format!("{} {b} @RAM;", ty.text()));
                self.bufs.push(Buf {
                    name: b.clone(),
                    ty,
                    dom: Dom::Ram,
                    destroyed: false,
                    exit_dom: None,
                    read_only: false,
                    depth,
                    split: None,
                    part_of: None,
                });
                if p.entry != Dom::Ram {
                    let f = Self::handoff_extern(ty, p.entry);
                    self.line(depth, &format!("{f}({b});"));
                    self.set_dom(&b, p.entry);
                }
                args.push((b, p.exit));
            }
            let names: Vec<&str> = args.iter().map(|(a, _)| a.as_str()).collect();
            self.line(depth, &format!("{}({});", h.name, names.join(", ")));
            for (a, exit) in args {
                self.set_dom(&a, exit);
            }
        }
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
    /// Returns true if the block ended in a `return`.
    fn scoped_block(&mut self, depth: usize, in_loop: bool) -> bool {
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
        returned
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
        for i in self.split_parents() {
            let parts = self.bufs[i].split.clone().unwrap();
            if parts
                .iter()
                .any(|p| self.bufs[nb..].iter().any(|b| &b.name == p))
            {
                self.rejoin(depth, i);
            }
        }
        for i in nb..self.bufs.len() {
            let b = &self.bufs[i];
            if b.usable() && b.dom != Dom::Ram {
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
            if !self.bufs[i].usable() || e.split.is_some() || self.bufs[i].dom == e.dom {
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
        let rejoin = self.rng.chance(0.7);
        for i in 0..self.handles.len() {
            if !self.handles[i].released {
                let h = self.handles[i].name.clone();
                self.line(depth, &format!("release {h};"));
                self.handles[i].released = true;
            }
        }
        if rejoin {
            for i in self.split_parents() {
                self.rejoin(depth, i);
            }
        }
        for i in 0..self.bufs.len() {
            let b = self.bufs[i].clone();
            if !b.usable() {
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

    // Interrupt handlers (in most programs): register writes, then a
    // signal for each of their events, every time they run.
    let mut hw = Hw::default();
    let mut handlers = String::new();
    if rng.chance(0.7) {
        let empty = Hw::default();
        let mut irqs: Vec<usize> = Vec::new();
        for i in 0..1 + rng.below(2) {
            let mut irq = rng.below(32);
            while irqs.contains(&irq) {
                irq = rng.below(32);
            }
            irqs.push(irq);
            let mut g = FnGen::new(&mut rng, cfg, &[], &empty);
            g.in_handler = true;
            // A handler deviates as a whole, in one way at most, so that
            // most programs with handlers can still be accepted.
            let deviate = g.hw_chaos();
            let wait_in_handler = deviate && g.rng.chance(0.4);
            g.clean = !deviate || wait_in_handler;
            for _ in 0..g.rng.below(3) {
                g.op_reg_write(0);
            }
            if wait_in_handler {
                // A handler waiting can never return.
                let e = EVENTS[g.rng.below(EVENTS.len())];
                g.line(0, &format!("wait {e};"));
            }
            for e in &EVENTS[..2] {
                if g.rng.chance(0.6) {
                    g.line(0, &format!("signal {e};"));
                    if !hw.signalled.contains(e) {
                        hw.signalled.push(e);
                    }
                }
            }
            for r in &g.written {
                if !hw.handler_regs.contains(r) {
                    hw.handler_regs.push(r);
                }
            }
            let body = std::mem::take(&mut g.out);
            let _ = writeln!(handlers, "\ninterrupt fn irq{i}() @ irq {irq} {{\n{body}}}");
        }
    }
    src.push_str(&handlers);

    let helpers: Vec<Helper> = (0..rng.below(3))
        .map(|i| Helper {
            name: format!("helper{i}"),
            params: (0..1 + rng.below(2))
                .map(|_| PARAM_KINDS[rng.below(PARAM_KINDS.len())])
                .collect(),
        })
        .collect();

    for h in &helpers {
        let mut g = FnGen::new(&mut rng, cfg, &[], &hw);
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
                depth: 0,
                split: None,
                part_of: None,
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

    let mut g = FnGen::new(&mut rng, cfg, &helpers, &hw);
    if !g.block(0, false) {
        g.call_every_helper(0);
        g.repair_all(0);
    }
    let _ = writeln!(src, "\nfn main() {{\n{}}}", g.out);
    src
}
