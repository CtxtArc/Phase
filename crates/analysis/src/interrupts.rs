//! Interrupts (v2.2): the whole-program rules for `interrupt fn`, `event`,
//! `signal`, `wait` and `critical`.
//!
//! A handler runs whenever its interrupt arrives, between any two
//! instructions of the code it interrupts (the *thread* code: `main` and
//! everything it calls). On a single core, handlers don't interrupt each
//! other here, and code inside `critical { }` can't be interrupted. So:
//!
//! - A field write is read-modify-write. In thread code, outside
//!   `critical`, a handler can run between the read and the write; if that
//!   handler writes the same register, its write is lost. Error.
//! - `wait` sleeps until a handler signals. In a handler -- or anything a
//!   handler calls -- nothing can interrupt it to signal: it never
//!   returns. Inside `critical`, interrupts are off: same. Error.
//! - A `return` inside `critical` would leave interrupts disabled. Error.
//! - Handlers are called by the hardware only, take nothing and return
//!   nothing, and each IRQ has one.
//!
//! What each function does (calls, register writes, waits) is collected
//! from its statements; which code is thread code and which is handler code
//! follows the call graph.

use crate::{err, AnalysisError};
use phase_ast::{Block, Expr, FnDecl, Item, Program, Span, Stmt};
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

/// Cortex-M has at most 240 external interrupts.
const MAX_IRQ: u32 = 240;

#[derive(Default)]
struct Facts {
    calls: Vec<(String, Span)>,
    /// (device, register, field, span, inside `critical`)
    writes: Vec<(String, String, Option<String>, Span, bool)>,
    /// (event, span, inside `critical`, the buffers it hands over)
    waits: Vec<(String, Span, bool, Vec<String>)>,
    signals: Vec<(String, Span)>,
    returns_in_critical: Vec<Span>,
}

/// `Dev.REG` / `Dev.REG.FIELD` (or an element of a register array,
/// `Dev.REG[i]`, v2.4), if `e` is one (devices are checked elsewhere; any
/// `A.B` shape is collected, and only real devices match the handler
/// writes they are compared with). The register is named with its index
/// when that is a literal, and `REG[?]` when it isn't known here.
fn register_of(e: &Expr) -> Option<(String, String, Option<String>)> {
    let p = phase_ast::reg_path(e)?;
    let reg = match p.index {
        None => p.reg.to_string(),
        Some(Expr::IntLit(i)) => format!("{}[{i}]", p.reg),
        Some(_) => format!("{}[?]", p.reg),
    };
    Some((p.device.to_string(), reg, p.field.map(str::to_string)))
}

/// Could registers `a` and `b` (as named by [`register_of`]) be the same?
fn same_register(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.split_once('['), b.split_once('[')) {
        (Some((x, i)), Some((y, j))) => x == y && (i == "?]" || j == "?]"),
        _ => false,
    }
}

fn walk_expr(e: &Expr, span: Span, critical: bool, f: &mut Facts) {
    match e {
        Expr::Call { callee, args } => {
            if callee == "volatile_write" {
                if let Some((d, r, field)) = args.first().and_then(register_of) {
                    f.writes.push((d, r, field, span, critical));
                }
            } else if callee != "volatile_read" {
                f.calls.push((callee.clone(), span));
            }
            for a in args {
                walk_expr(a, span, critical, f);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, span, critical, f);
            walk_expr(rhs, span, critical, f);
        }
        Expr::BitNot(x) => walk_expr(x, span, critical, f),
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                walk_expr(v, span, critical, f);
            }
        }
        _ => {}
    }
}

fn walk_block(b: &Block, critical: bool, f: &mut Facts) {
    for s in &b.stmts {
        let span = s.span;
        match &s.node {
            Stmt::VarDecl { init: Some(e), .. } | Stmt::Expr(e) | Stmt::Return(Some(e)) => {
                if critical && matches!(s.node, Stmt::Return(_)) {
                    f.returns_in_critical.push(span);
                }
                walk_expr(e, span, critical, f)
            }
            Stmt::Return(None) if critical => f.returns_in_critical.push(span),
            Stmt::Assign { value, .. } => walk_expr(value, span, critical, f),
            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                walk_expr(cond, span, critical, f);
                walk_block(then_block, critical, f);
                if let Some(e) = else_block {
                    walk_block(e, critical, f);
                }
            }
            Stmt::While { cond, body } => {
                walk_expr(cond, span, critical, f);
                walk_block(body, critical, f);
            }
            Stmt::Critical { body } => walk_block(body, true, f),
            Stmt::Signal { name } => f.signals.push((name.clone(), span)),
            Stmt::Wait { name, bufs } => f.waits.push((name.clone(), span, critical, bufs.clone())),
            _ => {}
        }
    }
}

/// Every function reachable from `roots` through calls, with the call
/// chain that first reached it (root first).
fn reachable<'p>(
    roots: &[&'p str],
    facts: &HashMap<&'p str, Facts>,
    fns: &HashMap<&'p str, &'p FnDecl>,
) -> HashMap<&'p str, Vec<&'p str>> {
    let mut seen: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut queue: VecDeque<&str> = VecDeque::new();
    for r in roots {
        seen.insert(r, vec![r]);
        queue.push_back(r);
    }
    while let Some(f) = queue.pop_front() {
        let path = seen[f].clone();
        for (callee, _) in facts.get(f).map(|x| x.calls.as_slice()).unwrap_or(&[]) {
            if let Some((name, _)) = fns.get_key_value(callee.as_str()) {
                if !seen.contains_key(name) {
                    let mut p = path.clone();
                    p.push(name);
                    seen.insert(name, p);
                    queue.push_back(name);
                }
            }
        }
    }
    seen
}

/// Check every interrupt rule; `devices` are the declared device names
/// (only writes to real devices count as handler writes).
pub(crate) fn check(program: &Program, devices: &HashSet<&str>, errors: &mut Vec<AnalysisError>) {
    // Events.
    let mut events: HashMap<&str, Span> = HashMap::new();
    let mut other_names: HashSet<&str> = HashSet::new();
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                other_names.insert(&f.name);
            }
            Item::ExternFn(f) => {
                other_names.insert(&f.name);
            }
            Item::Entity(e) => {
                other_names.insert(&e.name);
            }
            Item::State(s) => {
                other_names.insert(&s.name);
            }
            _ => {}
        }
    }
    for item in &program.items {
        let Item::Event(e) = item else { continue };
        if let Some(first) = events.get(e.name.as_str()) {
            errors.push(
                err(format!("event '{}' is declared more than once", e.name))
                    .at(e.span)
                    .note(*first, format!("'{}' is first declared here", e.name)),
            );
        } else if other_names.contains(e.name.as_str()) || devices.contains(e.name.as_str()) {
            errors.push(
                err(format!(
                    "'{}' is declared as an event and as something else",
                    e.name
                ))
                .at(e.span),
            );
        } else {
            events.insert(&e.name, e.span);
        }
    }

    // Handlers: shape and IRQ numbers.
    let fns: HashMap<&str, &FnDecl> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Fn(f) => Some((f.name.as_str(), f)),
            _ => None,
        })
        .collect();
    let mut irqs: HashMap<u32, &FnDecl> = HashMap::new();
    let mut handlers: Vec<&FnDecl> = Vec::new();
    for item in &program.items {
        let Item::Fn(f) = item else { continue };
        let Some((irq, irq_span)) = f.irq else {
            continue;
        };
        handlers.push(f);
        if f.name == "main" {
            errors.push(err("'main' cannot be an interrupt handler").at(f.name_span));
        }
        if !f.params.is_empty() || f.return_type.is_some() {
            errors.push(
                err(format!(
                    "interrupt handler '{}' must take no parameters and return nothing: the hardware calls it with none and ignores a result",
                    f.name
                ))
                .at(f.name_span),
            );
        }
        if irq >= MAX_IRQ {
            errors.push(
                err(format!(
                    "IRQ {irq} does not exist: Cortex-M interrupts are numbered 0 to {}",
                    MAX_IRQ - 1
                ))
                .at(irq_span),
            );
        } else if let Some(first) = irqs.get(&irq) {
            errors.push(
                err(format!(
                    "IRQ {irq} already has a handler, '{}'; an interrupt calls exactly one",
                    first.name
                ))
                .at(irq_span)
                .note(
                    first.name_span,
                    format!("'{}' handles IRQ {irq} here", first.name),
                ),
            );
        } else {
            irqs.insert(irq, f);
        }
    }

    // What every function does.
    let facts: HashMap<&str, Facts> = fns
        .iter()
        .map(|(name, f)| {
            let mut x = Facts::default();
            walk_block(&f.body, false, &mut x);
            (*name, x)
        })
        .collect();

    // Per-function rules: events exist, handlers aren't called, no
    // `return`/`wait` inside `critical`.
    let handler_names: HashSet<&str> = handlers.iter().map(|h| h.name.as_str()).collect();
    let mut names: Vec<&&str> = facts.keys().collect();
    names.sort();
    for name in names {
        let x = &facts[*name];
        for (callee, span) in &x.calls {
            if let Some(h) = handlers.iter().find(|h| &h.name == callee) {
                errors.push(
                    err(format!(
                        "'{callee}' is an interrupt handler: only the hardware calls it (IRQ {})",
                        h.irq.map_or(0, |(n, _)| n)
                    ))
                    .at(*span)
                    .note(h.name_span, format!("'{callee}' is declared here")),
                );
            }
        }
        let uses = x
            .signals
            .iter()
            .map(|(e, s)| (e, s))
            .chain(x.waits.iter().map(|(e, s, _, _)| (e, s)));
        for (ev, span) in uses {
            if !events.contains_key(ev.as_str()) {
                let mut known: Vec<&str> = events.keys().copied().collect();
                known.sort();
                errors.push(
                    err(format!(
                        "unknown event '{ev}'{}",
                        if known.is_empty() {
                            " (declare it: `event NAME;`)".to_string()
                        } else {
                            format!(" (events: {})", known.join(", "))
                        }
                    ))
                    .at(*span),
                );
            }
        }
        // v2.3: an event hands over one buffer (v2.4: one set of them).
        // Within a function, every `wait E for ..` names the same ones --
        // `wait ht for hi` after `wait ht for lo` is processing the wrong
        // half.
        let mut hands_over: HashMap<&str, (String, Span)> = HashMap::new();
        for (ev, span, _, bufs) in &x.waits {
            if bufs.is_empty() {
                continue;
            }
            let mut set: Vec<&str> = bufs.iter().map(String::as_str).collect();
            set.sort();
            set.dedup();
            let these = set.join(", ");
            let shown = |s: &str| {
                if s.contains(',') {
                    s.to_string()
                } else {
                    format!("'{s}'")
                }
            };
            match hands_over.get(ev.as_str()) {
                Some((first, first_span)) if *first != these => errors.push(
                    err(format!(
                        "`wait {ev} for {}`, but `{ev}` hands over {} earlier in this function: an event means one transfer is done, so it can only give back what that transfer had",
                        bufs.join(", "),
                        shown(first)
                    ))
                    .at(*span)
                    .note(*first_span, format!("`{ev}` hands over {} here", shown(first))),
                ),
                Some(_) => {}
                None => {
                    hands_over.insert(ev, (these, *span));
                }
            }
        }
        for (ev, span, critical, _) in &x.waits {
            if *critical {
                errors.push(
                    err(format!(
                        "`wait {ev}` inside `critical` would never return: interrupts are off, so no handler can signal it"
                    ))
                    .at(*span),
                );
            }
        }
        for span in &x.returns_in_critical {
            errors.push(
                err("`return` inside `critical` would leave interrupts disabled; return after the block")
                    .at(*span),
            );
        }
    }

    // Handler code: no `wait`.
    let handler_roots: Vec<&str> = handlers.iter().map(|h| h.name.as_str()).collect();
    let in_handlers = reachable(&handler_roots, &facts, &fns);
    let mut reported_waits: HashSet<Span> = HashSet::new();
    for (f, path) in sorted(&in_handlers) {
        for (ev, span, _, _) in &facts[f].waits {
            if !reported_waits.insert(*span) {
                continue;
            }
            let via = if path.len() > 1 {
                format!(
                    " (called from interrupt handler '{}': {})",
                    path[0],
                    path.join(" -> ")
                )
            } else {
                String::new()
            };
            errors.push(
                err(format!(
                    "`wait {ev}` in interrupt handler code would never return: nothing can interrupt a handler to signal it{via}"
                ))
                .at(*span),
            );
        }
    }

    // v2.3: a `wait` on an event nothing signals never returns. Signals
    // count from code that runs: `main`, the handlers, and what they call.
    let thread_roots: Vec<&str> = fns.keys().copied().filter(|n| *n == "main").collect();
    let in_thread = reachable(&thread_roots, &facts, &fns);
    let signalled: HashSet<&str> = in_thread
        .keys()
        .chain(in_handlers.keys())
        .flat_map(|f| facts[f].signals.iter().map(|(e, _)| e.as_str()))
        .collect();
    let mut names: Vec<&&str> = facts.keys().collect();
    names.sort();
    for name in names {
        for (ev, span, _, _) in &facts[*name].waits {
            if events.contains_key(ev.as_str())
                && !signalled.contains(ev.as_str())
                && !reported_waits.contains(span)
            {
                errors.push(
                    err(format!(
                        "`wait {ev}` never returns: nothing signals '{ev}' (an interrupt handler raises it with `signal {ev};`)"
                    ))
                    .at(*span)
                    .note(events[ev.as_str()], format!("'{ev}' is declared here")),
                );
            }
        }
    }

    // Thread code: no unprotected read-modify-write of a register a handler
    // writes.
    let mut handler_writes: Vec<(&str, &str, &str, Span)> = Vec::new();
    for (f, path) in sorted(&in_handlers) {
        for (d, r, _, span, _) in &facts[f].writes {
            if devices.contains(d.as_str())
                && !handler_writes
                    .iter()
                    .any(|(hd, hr, _, _)| hd == d && hr == r)
            {
                handler_writes.push((d.as_str(), r.as_str(), path[0], *span));
            }
        }
    }
    let mut reported: BTreeSet<(u32, u32, u32)> = BTreeSet::new();
    for (f, _) in sorted(&in_thread) {
        if handler_names.contains(f) {
            continue; // its own code isn't interrupted by handlers
        }
        for (d, r, field, span, critical) in &facts[f].writes {
            let (Some(field), false) = (field, *critical) else {
                continue;
            };
            let Some((_, hr, h, hspan)) = handler_writes
                .iter()
                .find(|(hd, hr, _, _)| hd == d && same_register(hr, r))
            else {
                continue;
            };
            if !reported.insert((span.file, span.line, span.col)) {
                continue;
            }
            let irq = fns.get(h).and_then(|x| x.irq).map_or(0, |(n, _)| n);
            errors.push(
                err(format!(
                    "writing field '{d}.{r}.{field}' reads '{d}.{r}', changes the field and writes it back -- and interrupt handler '{h}' (IRQ {irq}) also writes '{d}.{hr}': if it runs in between, its write is lost; do this inside `critical {{ }}`"
                ))
                .at(*span)
                .note(*hspan, format!("'{h}' writes '{d}.{hr}' here")),
            );
        }
    }
}

/// Map entries in a stable order, for deterministic diagnostics.
fn sorted<'a, 'p>(m: &'a HashMap<&'p str, Vec<&'p str>>) -> Vec<(&'p str, &'a Vec<&'p str>)> {
    let mut v: Vec<(&str, &Vec<&str>)> = m.iter().map(|(k, p)| (*k, p)).collect();
    v.sort_by_key(|(k, _)| *k);
    v
}

#[cfg(test)]
mod tests {
    use crate::{analyze, AnalysisError};

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[track_caller]
    fn rejects(src: &str, needle: &str) {
        let errs = check(src).expect_err("expected a rejection");
        assert!(
            errs.iter().any(|e| e.message.contains(needle)),
            "no error containing {needle:?} in {:#?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    const TIMER: &str = r#"
        device Timer @ 0x40000000 {
            CTRL: read write u32 @ 0x00 = 0 { ENABLE: bit 0, IRQ_EN: bit 3 }
            INT:  read write u32 @ 0x0C { PENDING: bit 0 }
        }
        event tick;
    "#;

    fn with(body: &str) -> String {
        format!("{TIMER}\n{body}")
    }

    #[test]
    fn a_handler_an_event_and_a_wait_are_accepted() {
        check(&with(
            "interrupt fn on_timer() @ irq 8 { volatile_write(Timer.INT.PENDING, 1); signal tick; }
             fn main() { volatile_write(Timer.CTRL, 9); wait tick; volatile_write(Timer.CTRL.ENABLE, 0); }",
        ))
        .unwrap();
    }

    #[test]
    fn a_field_write_racing_a_handler_needs_critical() {
        let handler = "interrupt fn on_timer() @ irq 8 { volatile_write(Timer.CTRL.IRQ_EN, 0); signal tick; }";
        rejects(
            &with(&format!("{handler} fn main() {{ volatile_write(Timer.CTRL.ENABLE, 1); }}")),
            "writing field 'Timer.CTRL.ENABLE' reads 'Timer.CTRL', changes the field and writes it back -- and interrupt handler 'on_timer' (IRQ 8) also writes 'Timer.CTRL'",
        );
        // Through a helper main calls.
        rejects(
            &with(&format!(
                "{handler} fn start() {{ volatile_write(Timer.CTRL.ENABLE, 1); }} fn main() {{ start(); }}"
            )),
            "writing field 'Timer.CTRL.ENABLE'",
        );
        // Protected, a whole write, or a register the handler doesn't write.
        check(&with(&format!(
            "{handler} fn main() {{ critical {{ volatile_write(Timer.CTRL.ENABLE, 1); }} volatile_write(Timer.CTRL, 1); volatile_write(Timer.INT.PENDING, 1); }}"
        )))
        .unwrap();
        // A helper only the handler calls runs with interrupts already
        // taken: its field writes can't be interrupted by that handler.
        check(&with(
            "fn ack() { volatile_write(Timer.CTRL.IRQ_EN, 0); }
             interrupt fn on_timer() @ irq 8 { ack(); }
             fn main() { critical { volatile_write(Timer.CTRL.ENABLE, 1); } }",
        ))
        .unwrap();
    }

    #[test]
    fn waiting_where_nothing_can_signal_is_rejected() {
        rejects(
            &with("interrupt fn on_timer() @ irq 8 { wait tick; } fn main() {}"),
            "`wait tick` in interrupt handler code would never return",
        );
        rejects(
            &with("fn settle() { wait tick; } interrupt fn on_timer() @ irq 8 { settle(); } fn main() {}"),
            "(called from interrupt handler 'on_timer': on_timer -> settle)",
        );
        rejects(
            &with("fn main() { critical { wait tick; } }"),
            "`wait tick` inside `critical` would never return",
        );
        rejects(
            &with("fn main() { critical { return; } }"),
            "`return` inside `critical` would leave interrupts disabled",
        );
    }

    #[test]
    fn handlers_are_shaped_and_numbered_for_the_hardware() {
        rejects(
            &with("interrupt fn h() @ irq 8 { } fn main() { h(); }"),
            "'h' is an interrupt handler: only the hardware calls it (IRQ 8)",
        );
        rejects(
            &with("interrupt fn a() @ irq 8 { } interrupt fn b() @ irq 8 { } fn main() {}"),
            "IRQ 8 already has a handler, 'a'",
        );
        rejects(
            &with("interrupt fn a() @ irq 240 { } fn main() {}"),
            "IRQ 240 does not exist",
        );
        rejects(
            &with("interrupt fn a(x: u32) @ irq 3 { } fn main() {}"),
            "must take no parameters and return nothing",
        );
        rejects(
            &with("interrupt fn main() @ irq 3 { }"),
            "'main' cannot be an interrupt handler",
        );
    }

    #[test]
    fn events_are_declared_once() {
        rejects(
            &with("fn main() { signal tock; }"),
            "unknown event 'tock' (events: tick)",
        );
        rejects(
            "fn main() { wait e; }",
            "unknown event 'e' (declare it: `event NAME;`)",
        );
        rejects(
            &with("event tick; fn main() {}"),
            "event 'tick' is declared more than once",
        );
        rejects(
            "event main; fn main() {}",
            "'main' is declared as an event and as something else",
        );
    }

    const RING: &str = r#"
        event ht;
        event tc;
        interrupt fn on_dma() @ irq 11 { signal ht; signal tc; }
        extern fn start(lo: write buffer<u8, 4> @RAM -> @DMA, hi: write buffer<u8, 4> @RAM -> @DMA);
        extern fn use_it(b: read buffer<u8, 4> @RAM);
    "#;

    fn ring(body: &str) -> String {
        format!(
            "{RING}\nfn main() {{ buffer<u8, 8> r @RAM; split r into lo, hi; start(lo, hi); {body} join lo, hi into r; }}"
        )
    }

    #[test]
    fn an_interrupt_hands_a_buffer_back_with_wait_for() {
        check(&ring(
            "let n: u32 = 0;
             while n < 3 {
                 wait ht for lo; use_it(lo); move lo -> @DMA;
                 wait tc for hi; use_it(hi); move hi -> @DMA;
                 n = n + 1;
             }
             wait ht for lo; wait tc for hi;",
        ))
        .unwrap();
        // Without the wait, `lo` is still the engine's.
        rejects(&ring("use_it(lo); sync(lo); sync(hi);"), "'lo'");
    }

    #[test]
    fn an_event_hands_back_one_buffer() {
        rejects(
            &ring(
                "wait ht for lo; move lo -> @DMA; wait ht for hi; wait ht for lo; wait tc for hi;",
            ),
            "`wait ht for hi`, but `ht` hands over 'lo' earlier in this function",
        );
        // Waiting for a buffer the hardware doesn't have.
        rejects(
            &ring("wait ht for lo; wait ht for lo; wait tc for hi;"),
            "`wait ht for lo`, but 'lo' is in @RAM: the hardware doesn't have it",
        );
    }

    #[test]
    fn a_wait_nothing_signals_is_rejected() {
        rejects(
            "event done; event other; interrupt fn h() @ irq 3 { signal other; } fn main() { wait done; }",
            "`wait done` never returns: nothing signals 'done'",
        );
        // A signal in a function nothing calls doesn't count.
        rejects(
            "event done; fn unused() { signal done; } fn main() { wait done; }",
            "`wait done` never returns",
        );
        // From a helper a handler calls, it does.
        check(
            "event done; fn raise() { signal done; } interrupt fn h() @ irq 3 { raise(); } fn main() { wait done; }",
        )
        .unwrap();
    }
}
