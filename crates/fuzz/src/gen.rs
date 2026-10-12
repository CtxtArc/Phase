//! Random PHASE program generator.
//!
//! Programs are always syntactically valid and only use declared names
//! (otherwise nearly everything would be rejected for boring reasons). Each
//! operation is chosen against a rough model of every buffer's domain and
//! borrows, and is *usually* consistent with it -- so a useful fraction of
//! programs pass `phase check` -- but a small, tunable fraction of choices
//! ignore the model, and blocks sometimes skip the "repair" that would put
//! things back. Those are the programs that probe the analyzer's edges.

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

#[derive(Clone, Debug)]
struct Buf {
    name: String,
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
    write: bool,
    released: bool,
}

/// One parameter shape a generated helper can have.
#[derive(Clone, Copy, Debug)]
struct ParamKind {
    text: &'static str,
    entry: Dom,
    exit: Dom,
    read_only: bool,
}

const PARAM_KINDS: &[ParamKind] = &[
    ParamKind {
        text: "buffer<u8, 8> @RAM",
        entry: Dom::Ram,
        exit: Dom::Ram,
        read_only: false,
    },
    ParamKind {
        text: "read buffer<u8, 8> @RAM",
        entry: Dom::Ram,
        exit: Dom::Ram,
        read_only: true,
    },
    ParamKind {
        text: "buffer<u8, 8> @DMA -> @RAM",
        entry: Dom::Dma,
        exit: Dom::Ram,
        read_only: false,
    },
    ParamKind {
        text: "write buffer<u8, 8> @RAM -> @DMA",
        entry: Dom::Ram,
        exit: Dom::Dma,
        read_only: false,
    },
    ParamKind {
        text: "buffer<u8, 8> @DMA",
        entry: Dom::Dma,
        exit: Dom::Dma,
        read_only: false,
    },
];

#[derive(Clone, Debug)]
struct Helper {
    name: String,
    params: Vec<ParamKind>,
}

/// The fixed extern vocabulary every generated program declares.
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
";

struct FnGen<'a> {
    rng: &'a mut Rng,
    cfg: Config,
    helpers: &'a [Helper],
    out: String,
    counter: usize,
    bufs: Vec<Buf>,
    handles: Vec<Handle>,
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
    fn arg_for(&mut self, dom: Dom, write: bool, handoff: bool) -> Option<String> {
        if self.chaos() {
            return self.any_buf();
        }
        let mut options: Vec<String> = self
            .bufs
            .iter()
            .filter(|b| !b.destroyed && b.dom == dom)
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
            for h in self.handles.iter().filter(|h| !h.released) {
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
        match self.rng.below(100) {
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
        self.line(depth, &format!("buffer<u8, 8> {name} @{};", dom.name()));
        self.bufs.push(Buf {
            name,
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
        match self.rng.below(7) {
            0 => {
                if let Some(x) = self.arg_for(Dom::Ram, false, false) {
                    self.line(depth, &format!("look({x});"));
                }
            }
            1 => {
                if let Some(x) = self.arg_for(Dom::Ram, true, false) {
                    self.line(depth, &format!("poke({x});"));
                }
            }
            2 => {
                let a = self.arg_for(Dom::Ram, false, false);
                let b = self.arg_for(Dom::Ram, true, false);
                if let (Some(a), Some(b)) = (a, b) {
                    self.line(depth, &format!("mix({a}, {b});"));
                }
            }
            3 => {
                let a = self.arg_for(Dom::Ram, false, false);
                let b = self.arg_for(Dom::Ram, false, false);
                if let (Some(a), Some(b)) = (a, b) {
                    self.line(depth, &format!("cmp({a}, {b});"));
                }
            }
            4 => {
                if let Some(x) = self.arg_for(Dom::Ram, true, true) {
                    self.line(depth, &format!("start({x});"));
                    self.set_dom(&x, Dom::Dma);
                }
            }
            5 => {
                if let Some(x) = self.arg_for(Dom::Dma, false, true) {
                    let f = if self.rng.chance(0.5) {
                        "finish"
                    } else {
                        "kick"
                    };
                    self.line(depth, &format!("{f}({x});"));
                    if f == "finish" {
                        self.set_dom(&x, Dom::Ram);
                    }
                }
            }
            _ => {
                if let Some(x) = self.arg_for(Dom::Ram, true, true) {
                    self.line(depth, &format!("emit({x});"));
                    self.set_dom(&x, Dom::Device);
                }
            }
        }
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
        let name = self.fresh("h");
        let mode = if write { "write" } else { "read" };
        self.line(depth, &format!("let {name} = borrow {target} {mode};"));
        self.handles.push(Handle {
            name,
            target,
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
        let a = self.rng.below(c.len());
        let mut b = self.rng.below(c.len());
        if a == b {
            b = (b + 1) % c.len();
        }
        let (a, b) = (c[a].clone(), c[b].clone());
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
            let Some(a) = self.arg_for(p.entry, !p.read_only, handoff) else {
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
        let saved = (self.bufs.clone(), self.handles.clone());
        self.scoped_block(depth + 1, in_loop);
        let then_state = (self.bufs.clone(), self.handles.clone());
        (self.bufs, self.handles) = saved;
        if self.rng.chance(0.6) {
            self.line(depth, "} else {");
            self.scoped_block(depth + 1, in_loop);
        }
        self.line(depth, "}");
        // Continue from one of the two branches' views (they should agree
        // if the program is going to be accepted).
        if self.rng.chance(0.5) {
            (self.bufs, self.handles) = then_state;
        }
    }

    fn op_while(&mut self, depth: usize) {
        let n = self.fresh("n");
        let lim = self.fresh("l");
        self.line(depth, &format!("let {n}: u32 = 0;"));
        self.line(depth, &format!("let {lim}: u32 = count();"));
        self.line(depth, &format!("while {n} < {lim} {{"));
        let entry = self.bufs.clone();
        // (no `return` inside loop bodies, so the body always falls through)
        self.scoped_block(depth + 1, true);
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
        let returned = self.block(depth, in_loop);
        if !returned && !self.rng.chance(self.cfg.skip_repair) {
            self.repair_locals(nb, nh, depth);
        }
        self.bufs.truncate(nb);
        self.handles.truncate(nh);
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
            match e.dom {
                Dom::Ram => {}
                Dom::Dma => {
                    self.line(depth, &format!("start({name});"));
                    self.bufs[i].dom = Dom::Dma;
                }
                Dom::Device => {
                    self.line(depth, &format!("emit({name});"));
                    self.bufs[i].dom = Dom::Device;
                }
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
                self.line(depth, &format!("start({});", b.name));
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
        };
        let mut params = Vec::new();
        for (i, p) in h.params.iter().enumerate() {
            let name = format!("p{i}");
            params.push(format!("{name}: {}", p.text));
            g.bufs.push(Buf {
                name,
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
    };
    if !g.block(0, false) {
        g.repair_all(0);
    }
    let _ = writeln!(src, "\nfn main() {{\n{}}}", g.out);
    src
}
