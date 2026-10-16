//! The PHASE safety analyzer: physical domains, ownership and borrows,
//! typestate, control-flow merging, and MMIO access rules.
//!
//! [`analyze`] checks every function body independently against the
//! signatures of every function in the program. It never stops at the
//! first error: an erroring statement is treated as a no-op and checking
//! continues, so one mistake does not cascade into unrelated errors.
//!
//! What is enforced (spec Part 4):
//!
//! * **Domains** -- an entity is only accepted where its *current* domain
//!   matches the parameter's; `move`/`sync` follow the transition table
//!   (spec 2.4); `@DMA`/`@DEVICE`-owned entities cannot be borrowed,
//!   destroyed, or have their fields read until synced.
//! * **Ownership and borrows** -- many readers or one writer; the owner
//!   cannot be used, moved, synced or destroyed while borrowed; released
//!   handles are dead; the same storage may appear twice in one call only
//!   through read borrows; entities cannot be copied with `let`.
//! * **Lifetimes** -- no use after `destroy`; at every `return` (explicit
//!   or implicit) no local may still be hardware-owned, no borrow may be
//!   live, and parameters must be back in their entry domain.
//! * **Typestate** -- a value's state comes only from a function's
//!   declared return state (or the initial state, for a fresh value); an
//!   annotation is checked against that, never trusted. Values are linear:
//!   passing one to a state-requiring parameter consumes it.
//! * **Control flow** -- each `if` arm starts from the same state and
//!   pre-existing entities must agree at the join (arms that `return` do
//!   not reach it); a `while` body must leave every entity as it found it,
//!   which makes a single pass a sound loop invariant.
//! * **MMIO** -- `volatile_read`/`volatile_write` take an `@MMIO` register
//!   reference as their first argument.

use phase_ast::{
    eval_const, uint_max, BinOp, BorrowMode, BufferLen, ConstDecl, DeviceDecl, DeviceTypeDecl,
    Domain, Expr, FieldKind, FnDecl, Item, Program, RegDecl, RegField, Span, Stmt, TypeExpr,
};
use std::collections::{HashMap, HashSet};

mod interrupts;

/// How serious a diagnostic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A safety violation: the program is rejected.
    Error,
    /// Accepted, but relies on something PHASE can't check (e.g. an
    /// unannotated parameter); expected to become an error in 2.0.
    Warning,
}

/// The full result of analysis: errors reject the program, warnings don't.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    pub errors: Vec<AnalysisError>,
    pub warnings: Vec<AnalysisError>,
}

/// One statement as the analyzer saw it (v2.3, for the language server's
/// hover): what every local in the function was just before it, and just
/// after. A statement inside a loop or a branch can appear more than once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStep {
    pub fn_name: String,
    pub span: Span,
    /// An `if`, `while` or `critical`: its own statements have steps too.
    pub compound: bool,
    pub before: Vec<(String, String)>,
    pub after: Vec<(String, String)>,
}

/// Analyze `program`, recording every statement's [`TraceStep`].
pub fn trace(program: &Program) -> (Report, Vec<TraceStep>) {
    let mut steps = Vec::new();
    let report = analyze_impl(program, Some(&mut steps));
    (report, steps)
}

/// One safety violation (or, with [`Severity::Warning`], a concern).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisError {
    pub severity: Severity,
    pub message: String,
    /// Where the violation happens: the statement (or declaration) that
    /// breaks the rule. `None` only for errors with no single location.
    pub span: Option<Span>,
    /// The entity the error is about, if any -- lets a renderer narrow its
    /// underline from the whole statement to that name.
    pub subject: Option<String>,
    /// Related locations that explain the error ("moved to @DMA here").
    pub notes: Vec<Note>,
}

/// A secondary location attached to an [`AnalysisError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub span: Span,
    pub message: String,
}

impl AnalysisError {
    fn about(mut self, name: &str) -> Self {
        self.subject = Some(name.to_string());
        self
    }

    fn note(mut self, span: Span, message: impl Into<String>) -> Self {
        self.notes.push(Note {
            span,
            message: message.into(),
        });
        self
    }

    fn note_opt(self, n: Option<&(Span, String)>) -> Self {
        match n {
            Some((span, msg)) => self.note(*span, msg.clone()),
            None => self,
        }
    }

    fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

fn err(msg: impl Into<String>) -> AnalysisError {
    AnalysisError {
        severity: Severity::Error,
        message: msg.into(),
        span: None,
        subject: None,
        notes: Vec::new(),
    }
}

/// No rule warns at the moment (the last warning became an error in 2.0);
/// the severity stays, as does its rendering in every output format.
#[allow(dead_code)]
fn warn(msg: impl Into<String>) -> AnalysisError {
    AnalysisError {
        severity: Severity::Warning,
        ..err(msg)
    }
}

/// `(machine name, state name)`, e.g. `("Packet", "Received")`.
type Typestate = (String, String);

/// name -> resolved per-parameter domain / typestate requirements, plus
/// the resolved return typestate (if the return type is stateful).
#[derive(Debug, Clone)]
struct FnSig {
    param_names: Vec<String>,
    /// Declared parameter types (for checking buffer arguments).
    param_types: Vec<TypeExpr>,
    /// Declared `read` / `write` access per parameter (`None`: no mode).
    param_access: Vec<Option<BorrowMode>>,
    param_domains: Vec<Option<Domain>>,
    /// `Some(to)` where a parameter is declared `@FROM -> @TO`: the call
    /// leaves the argument in `to`.
    param_domains_after: Vec<Option<Domain>>,
    param_states: Vec<Option<Typestate>>,
    return_state: Option<Typestate>,
}

/// Validate a `TypeExpr` that may be a `Name<State>` typestate type against
/// the program's declared state machines, returning the resolved
/// `(machine, state)` pair if it's stateful and valid. Returns `None` for
/// non-stateful types silently; pushes an error and returns `None` for a
/// stateful type referencing an unknown machine or unknown state.
fn resolve_stateful(
    ty: &TypeExpr,
    machines: &HashMap<&str, HashSet<&str>>,
    errors: &mut Vec<AnalysisError>,
) -> Option<Typestate> {
    let TypeExpr::Stateful { name, state } = ty else {
        return None;
    };
    match machines.get(name.as_str()) {
        None => {
            errors.push(err(format!("unknown typestate '{name}'")));
            None
        }
        Some(states) => {
            if states.contains(state.as_str()) {
                Some((name.clone(), state.clone()))
            } else {
                errors.push(err(format!(
                    "unknown state '{state}' for typestate '{name}'"
                )));
                None
            }
        }
    }
}

/// Analyze a whole program. Returns `Ok(())` if every function body is
/// physically consistent, otherwise every violation found (the checker
/// does not stop at the first error — it keeps going, treating the
/// erroring statement as a no-op, so one mistake doesn't cascade into a
/// wall of unrelated-looking follow-on errors).
pub fn analyze(program: &Program) -> Result<(), Vec<AnalysisError>> {
    let report = analyze_full(program);
    if report.errors.is_empty() {
        Ok(())
    } else {
        Err(report.errors)
    }
}

/// Like [`analyze`], but also returns warnings (which never reject a
/// program).
pub fn analyze_full(program: &Program) -> Report {
    analyze_impl(program, None)
}

fn analyze_impl(program: &Program, mut steps: Option<&mut Vec<TraceStep>>) -> Report {
    let mut errors = Vec::new();

    let entity_domains: HashMap<&str, Option<Domain>> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Entity(e) => Some((e.name.as_str(), e.domain)),
            _ => None,
        })
        .collect();

    // Build the state-machine table, flagging duplicate machine names and
    // duplicate state names within one machine as we go.
    let mut state_machines: HashMap<&str, HashSet<&str>> = HashMap::new();
    let mut initial_states: HashMap<&str, &str> = HashMap::new();
    for item in &program.items {
        if let Item::State(s) = item {
            if let Some(first) = s.states.first() {
                initial_states
                    .entry(s.name.as_str())
                    .or_insert(first.as_str());
            }
            if state_machines.contains_key(s.name.as_str()) {
                errors.push(err(format!(
                    "typestate '{}' is declared more than once",
                    s.name
                )));
                continue;
            }
            let mut states = HashSet::new();
            for st in &s.states {
                if !states.insert(st.as_str()) {
                    errors.push(err(format!(
                        "typestate '{}' declares state '{}' more than once",
                        s.name, st
                    )));
                }
            }
            state_machines.insert(s.name.as_str(), states);
        }
    }

    // name -> resolved signature (domains + typestates for params/return).
    // Signatures are validated exactly once here, at declaration site, not
    // re-validated at every call site.
    let mut fn_sigs: HashMap<&str, FnSig> = HashMap::new();
    let build_sig = |name: &str,
                     params: &[phase_ast::Param],
                     return_type: &Option<TypeExpr>,
                     errors: &mut Vec<AnalysisError>|
     -> FnSig {
        // A declared domain change must be a transition the hardware model
        // allows (spec §2.4) -- the same table `move` and `sync` obey.
        for p in params {
            if let (Some(from), Some(to)) = (p.domain, p.domain_after) {
                let msg = if from == to {
                    Some(format!(
                        "parameter '{}' of '{name}' declares @{} -> @{}, which changes nothing; drop the `-> @{}`",
                        p.name,
                        from.name(),
                        to.name(),
                        to.name()
                    ))
                } else if !(legal_move(from, to) || (legal_sync_source(from) && to == Domain::Ram))
                {
                    Some(format!(
                        "no legal transition from @{} to @{} in the signature of '{name}' (parameter '{}')",
                        from.name(),
                        to.name(),
                        p.name
                    ))
                } else {
                    None
                };
                if let Some(msg) = msg {
                    // Underline the whole parameter, `-> @TO` included.
                    errors.push(err(msg).at(p.span));
                }
            }
        }
        for p in params {
            let Some(mode) = p.access else { continue };
            let word = if mode == BorrowMode::Read {
                "read"
            } else {
                "write"
            };
            let is_entity_type = match &p.ty {
                TypeExpr::Buffer { .. } | TypeExpr::Stateful { .. } => true,
                TypeExpr::Named(n) => entity_domains.contains_key(n.as_str()),
            };
            if !is_entity_type {
                errors.push(
                    err(format!(
                        "parameter '{}' of '{name}' is declared `{word}`, but access modes only apply to buffers and entities",
                        p.name
                    ))
                    .at(p.span),
                );
            } else if mode == BorrowMode::Read && p.domain_after.is_some() {
                errors.push(
                    err(format!(
                        "parameter '{}' of '{name}' is declared `read` but also hands the buffer to hardware (`-> @..`), which may write it; drop `read`",
                        p.name
                    ))
                    .at(p.span),
                );
            }
        }
        FnSig {
            param_names: params.iter().map(|p| p.name.clone()).collect(),
            param_types: params.iter().map(|p| p.ty.clone()).collect(),
            param_access: params.iter().map(|p| p.access).collect(),
            param_domains: params.iter().map(|p| p.domain).collect(),
            param_domains_after: params.iter().map(|p| p.domain_after).collect(),
            param_states: params
                .iter()
                .map(|p| resolve_stateful(&p.ty, &state_machines, errors))
                .collect(),
            return_state: return_type
                .as_ref()
                .and_then(|t| resolve_stateful(t, &state_machines, errors)),
        }
    };
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                let sig = build_sig(&f.name, &f.params, &f.return_type, &mut errors);
                fn_sigs.insert(f.name.as_str(), sig);
            }
            Item::ExternFn(f) => {
                let sig = build_sig(&f.name, &f.params, &f.return_type, &mut errors);
                fn_sigs.insert(f.name.as_str(), sig);
            }
            _ => {}
        }
    }

    // `buffer<T, _>` (run-time length) is only meaningful for parameters,
    // whose length travels with them; storage needs a concrete length.
    let any_len = |t: &TypeExpr| {
        matches!(
            t,
            TypeExpr::Buffer {
                len: BufferLen::Any,
                ..
            }
        )
    };
    for item in &program.items {
        match item {
            Item::Entity(e) => {
                for f in e.fields.iter().filter(|f| any_len(&f.ty)) {
                    errors.push(err(format!(
                        "field '{}' of entity '{}' has length `_`; only parameters may have a run-time length",
                        f.name, e.name
                    )));
                }
            }
            Item::Fn(f) if f.return_type.as_ref().is_some_and(any_len) => {
                errors.push(
                    err(format!(
                        "'{}' returns a buffer of length `_`; only parameters may have a run-time length",
                        f.name
                    ))
                    .at(f.name_span),
                );
            }
            Item::ExternFn(f) if f.return_type.as_ref().is_some_and(any_len) => {
                errors.push(
                    err(format!(
                        "'{}' returns a buffer of length `_`; only parameters may have a run-time length",
                        f.name
                    ))
                    .at(f.name_span),
                );
            }
            _ => {}
        }
    }

    let devices = check_devices(program, &mut errors);
    let consts = check_consts(program, &devices, &mut errors);
    interrupts::check(program, &devices.keys().copied().collect(), &mut errors);

    for item in &program.items {
        match item {
            // `parse` leaves imports in place; only loading from files
            // (`phase_parser::load`) can resolve them.
            Item::Import(imp) => errors.push(
                err(format!(
                    "cannot resolve `import \"{}\"` here: imports are resolved when compiling a file",
                    imp.path
                ))
                .at(imp.span),
            ),
            // The program's entry point belongs to the file being compiled.
            Item::Fn(f) if f.name == "main" && f.name_span.file != 0 => errors.push(
                err("an imported file defines 'main'; only the file being compiled may")
                    .at(f.name_span),
            ),
            _ => {}
        }
    }

    let mut seen_fns: HashMap<&str, Span> = HashMap::new();
    for item in &program.items {
        let (name, span) = match item {
            Item::Fn(f) => (&f.name, f.name_span),
            Item::ExternFn(f) => (&f.name, f.name_span),
            _ => continue,
        };
        if let Some(first) = seen_fns.get(name.as_str()) {
            errors.push(
                err(format!("function '{name}' is declared more than once"))
                    .at(span)
                    .about(name)
                    .note(*first, format!("'{name}' is first declared here")),
            );
        } else {
            seen_fns.insert(name.as_str(), span);
        }
    }

    for item in &program.items {
        if let Item::Fn(f) = item {
            let mut checker = FnChecker::new(
                &f.name,
                &devices,
                &consts,
                &entity_domains,
                &fn_sigs,
                &state_machines,
                &initial_states,
            );
            if steps.is_some() {
                checker.trace = Some(Vec::new());
            }
            checker.check_fn(f, &mut errors);
            if let (Some(all), Some(mine)) = (steps.as_deref_mut(), checker.trace.take()) {
                all.extend(mine);
            }
        }
    }

    // One mistake can trip the same rule more than once (e.g. the same
    // entity passed twice in one call); report each distinct error once.
    let mut seen = HashSet::new();
    errors.retain(|e| seen.insert(e.message.clone()));

    let (warnings, errors) = errors
        .into_iter()
        .partition(|e| e.severity == Severity::Warning);
    Report { errors, warnings }
}

/// What the analyzer knows about one device: where it is, and its
/// registers (its own, or its type's).
#[derive(Clone, Copy)]
struct DeviceView<'p> {
    span: Span,
    base: u64,
    regs: &'p [RegDecl],
}

impl<'p> DeviceView<'p> {
    fn reg(&self, name: &str) -> Option<&'p RegDecl> {
        phase_ast::find_reg(self.regs, name)
    }

    /// Bytes from the base address to the end of the last register.
    fn extent(&self) -> u64 {
        self.regs
            .iter()
            .flat_map(RegDecl::elements)
            .filter_map(|r| Some(r.offset + u64::from(r.bits()? / 8)))
            .max()
            .unwrap_or(0)
    }
}

/// Validate every `device` register map (v2.0) and `device type` (v2.1),
/// and index the devices by name. A register is `u8`, `u16` or `u32`,
/// aligned to its width, and doesn't overlap another; a field lies within
/// its register and doesn't overlap another field; a reset value fits. An
/// instance names a declared type; a device's base address keeps its
/// registers aligned; and no two devices overlap in the address space.
/// Device and type names share the namespace of functions, entities and
/// typestate machines.
fn check_devices<'p>(
    program: &'p Program,
    errors: &mut Vec<AnalysisError>,
) -> HashMap<&'p str, DeviceView<'p>> {
    let mut other_names: HashSet<&str> = HashSet::new();
    for item in &program.items {
        match item {
            Item::Fn(f) => other_names.insert(f.name.as_str()),
            Item::ExternFn(f) => other_names.insert(f.name.as_str()),
            Item::Entity(e) => other_names.insert(e.name.as_str()),
            Item::State(s) => other_names.insert(s.name.as_str()),
            Item::Device(_)
            | Item::DeviceType(_)
            | Item::Event(_)
            | Item::Const(_)
            | Item::Import(_) => false,
        };
    }
    let mut types: HashMap<&str, &DeviceTypeDecl> = HashMap::new();
    for item in &program.items {
        let Item::DeviceType(t) = item else { continue };
        if let Some(first) = types.get(t.name.as_str()) {
            errors.push(
                err(format!(
                    "device type '{}' is declared more than once",
                    t.name
                ))
                .at(t.span)
                .note(first.span, format!("'{}' is first declared here", t.name)),
            );
            continue;
        }
        if other_names.contains(t.name.as_str()) {
            errors.push(
                err(format!(
                    "'{}' is declared as a device type and as a function, entity or typestate",
                    t.name
                ))
                .at(t.span),
            );
        }
        check_reg_map("device type", &t.name, &t.regs, errors);
        types.insert(t.name.as_str(), t);
    }
    let mut devices: HashMap<&str, DeviceView> = HashMap::new();
    let mut order: Vec<(&DeviceDecl, DeviceView)> = Vec::new();
    for item in &program.items {
        let Item::Device(d) = item else { continue };
        if let Some(first) = devices.get(d.name.as_str()) {
            errors.push(
                err(format!("device '{}' is declared more than once", d.name))
                    .at(d.span)
                    .note(first.span, format!("'{}' is first declared here", d.name)),
            );
            continue;
        }
        if other_names.contains(d.name.as_str()) || types.contains_key(d.name.as_str()) {
            errors.push(
                err(format!(
                    "'{}' is declared as a device and as a device type, function, entity or typestate",
                    d.name
                ))
                .at(d.span),
            );
        }
        let regs: &[RegDecl] = match &d.of_type {
            None => {
                check_reg_map("device", &d.name, &d.regs, errors);
                &d.regs
            }
            Some((ty, tspan)) => match types.get(ty.as_str()) {
                Some(t) => &t.regs,
                None => {
                    let mut known: Vec<&str> = types.keys().copied().collect();
                    known.sort();
                    errors.push(
                        err(format!(
                            "device '{}' is of type '{ty}', which is not declared{}",
                            d.name,
                            if known.is_empty() {
                                " (there are no device types)".to_string()
                            } else {
                                format!(" (device types: {})", known.join(", "))
                            }
                        ))
                        .at(*tspan),
                    );
                    continue;
                }
            },
        };
        let align = regs
            .iter()
            .filter_map(|r| r.bits())
            .map(|b| u64::from(b / 8))
            .max()
            .unwrap_or(1);
        if d.base % align != 0 {
            errors.push(
                err(format!(
                    "device '{}' is at {:#x}, which is not aligned to its {align}-byte registers",
                    d.name, d.base
                ))
                .at(d.span),
            );
        }
        let view = DeviceView {
            span: d.span,
            base: d.base,
            regs,
        };
        devices.insert(d.name.as_str(), view);
        order.push((d, view));
    }
    // Two devices sharing addresses: every write to one changes the other.
    for (i, (d, v)) in order.iter().enumerate() {
        let (start, end) = (v.base, v.base.saturating_add(v.extent()));
        if start == end {
            continue;
        }
        if let Some((e, w)) = order[..i].iter().find(|(_, w)| {
            let (s2, e2) = (w.base, w.base.saturating_add(w.extent()));
            s2 < e2 && start < e2 && s2 < end
        }) {
            errors.push(
                err(format!(
                    "devices '{}' ({:#x}..{:#x}) and '{}' ({:#x}..{:#x}) overlap",
                    d.name,
                    start,
                    end,
                    e.name,
                    w.base,
                    w.base + w.extent()
                ))
                .at(d.span)
                .note(e.span, format!("'{}' is declared here", e.name)),
            );
        }
    }
    devices
}

/// Validate one register map: the registers of a device or device type.
fn check_reg_map(what: &str, owner: &str, regs: &[RegDecl], errors: &mut Vec<AnalysisError>) {
    let mut placed: Vec<(&RegDecl, u64, u64)> = Vec::new();
    for r in regs {
        let full = format!("{owner}.{}", r.name);
        if let Some((first, _, _)) = placed.iter().find(|(p, _, _)| p.name == r.name) {
            errors.push(
                err(format!(
                    "{what} '{owner}' declares register '{}' more than once",
                    r.name
                ))
                .at(r.span)
                .note(first.span, format!("'{full}' is first declared here")),
            );
            continue;
        }
        let Some(bits) = r.bits() else {
            errors.push(
                err(format!(
                    "register '{full}' has type '{}'; a register is u8, u16 or u32",
                    r.ty
                ))
                .at(r.span),
            );
            continue;
        };
        let bytes = u64::from(bits / 8);
        // v2.4: an array's elements are placed one by one.
        let elements: Vec<(String, u64)> = match r.array {
            None => vec![(full.clone(), r.offset)],
            Some((0, _)) => {
                errors.push(err(format!("register array '{full}' has no registers")).at(r.span));
                Vec::new()
            }
            Some((_, step)) if step < bytes => {
                errors.push(
                    err(format!(
                        "register array '{full}' has a step of {step} bytes, less than its {bytes}-byte registers: they would overlap"
                    ))
                    .at(r.span),
                );
                Vec::new()
            }
            Some((n, step)) => (0..n)
                .map(|i| (format!("{full}[{i}]"), r.offset + u64::from(i) * step))
                .collect(),
        };
        let mut misaligned = false;
        for (ename, offset) in &elements {
            if offset % bytes != 0 && !misaligned {
                misaligned = true;
                errors.push(
                    err(format!(
                        "register '{ename}' is at offset {offset:#x}, which is not aligned to its {bytes} bytes"
                    ))
                    .at(r.span),
                );
            }
            let (start, end) = (*offset, offset.saturating_add(bytes));
            if let Some((other, _, _)) = placed.iter().find(|(_, s, e)| start < *e && *s < end) {
                errors.push(
                    err(format!(
                        "register '{ename}' (bytes {start:#x}..{end:#x}) overlaps '{owner}.{}'",
                        other.name
                    ))
                    .at(r.span)
                    .note(
                        other.span,
                        format!("'{owner}.{}' is declared here", other.name),
                    ),
                );
                break;
            }
        }
        for (_, offset) in &elements {
            placed.push((r, *offset, offset.saturating_add(bytes)));
        }
        let max = if bits == 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        if let Some(v) = r.reset.filter(|v| *v > max) {
            errors.push(
                err(format!(
                    "reset value {v:#x} does not fit in the {bits}-bit register '{full}'"
                ))
                .at(r.span),
            );
        }
        let mut fields: Vec<&RegField> = Vec::new();
        for f in &r.fields {
            if f.lo > f.hi {
                errors.push(
                    err(format!(
                        "field '{full}.{}' is `bits {}..={}`; the low bit comes first",
                        f.name, f.lo, f.hi
                    ))
                    .at(f.span),
                );
                continue;
            }
            if f.hi >= bits {
                errors.push(
                    err(format!(
                        "field '{full}.{}' uses bit {}, outside the {bits}-bit register",
                        f.name, f.hi
                    ))
                    .at(f.span),
                );
                continue;
            }
            if let Some(first) = fields.iter().find(|g| g.name == f.name) {
                errors.push(
                    err(format!(
                        "register '{full}' declares field '{}' more than once",
                        f.name
                    ))
                    .at(f.span)
                    .note(first.span, "first declared here"),
                );
                continue;
            }
            if let Some(g) = fields.iter().find(|g| f.lo <= g.hi && g.lo <= f.hi) {
                errors.push(
                    err(format!(
                        "fields '{full}.{}' and '{full}.{}' share bits",
                        g.name, f.name
                    ))
                    .at(f.span)
                    .note(g.span, format!("'{}' is declared here", g.name)),
                );
                continue;
            }
            fields.push(f);
        }
    }
}

/// `Dev.REG` or `Dev.REG.FIELD` as written.
struct DeviceRef<'e> {
    device: &'e str,
    reg: &'e str,
    /// `REG[i]` (v2.4).
    index: Option<&'e Expr>,
    field: Option<&'e str>,
}

impl DeviceRef<'_> {
    /// `Dev.REG`, `Dev.REG[i]`, as written (without the field).
    fn reg_text(&self) -> String {
        match self.index {
            None => format!("{}.{}", self.device, self.reg),
            Some(Expr::IntLit(i)) => format!("{}.{}[{i}]", self.device, self.reg),
            Some(Expr::Ident(n)) => format!("{}.{}[{n}]", self.device, self.reg),
            Some(_) => format!("{}.{}[..]", self.device, self.reg),
        }
    }
}

/// v2.4: every `const`: a unique name, an unsigned integer type, a
/// constant value that fits it. Returns the values of the valid ones.
fn check_consts(
    program: &Program,
    devices: &HashMap<&str, DeviceView>,
    errors: &mut Vec<AnalysisError>,
) -> HashMap<String, u64> {
    let mut decls: HashMap<&str, &ConstDecl> = HashMap::new();
    let mut other_names: HashSet<&str> = devices.keys().copied().collect();
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                other_names.insert(&f.name);
            }
            Item::ExternFn(f) => {
                other_names.insert(&f.name);
            }
            Item::Event(e) => {
                other_names.insert(&e.name);
            }
            Item::Entity(e) => {
                other_names.insert(&e.name);
            }
            _ => {}
        }
    }
    for item in &program.items {
        let Item::Const(c) = item else { continue };
        if let Some(first) = decls.get(c.name.as_str()) {
            errors.push(
                err(format!("constant '{}' is declared more than once", c.name))
                    .at(c.span)
                    .note(first.span, format!("'{}' is first declared here", c.name)),
            );
        } else if other_names.contains(c.name.as_str()) {
            errors.push(
                err(format!(
                    "'{}' is declared as a constant and as something else",
                    c.name
                ))
                .at(c.span),
            );
        } else {
            decls.insert(&c.name, c);
        }
    }

    // Evaluate each, following references (in any order) and stopping at
    // a cycle.
    fn eval<'p>(
        name: &'p str,
        decls: &HashMap<&'p str, &'p ConstDecl>,
        done: &mut HashMap<&'p str, Result<u64, String>>,
        visiting: &mut Vec<&'p str>,
    ) -> Result<u64, String> {
        if let Some(r) = done.get(name) {
            return r.clone();
        }
        if visiting.contains(&name) {
            return Err(format!("constant '{name}' is defined in terms of itself"));
        }
        let c = decls[name];
        visiting.push(name);
        // Resolve the constants this one uses first.
        let mut refs = Vec::new();
        collect_idents(&c.value, &mut refs);
        let mut resolved: HashMap<&str, Result<u64, String>> = HashMap::new();
        for r in refs {
            if let Some((key, _)) = decls.get_key_value(r) {
                let v = eval(key, decls, done, visiting);
                resolved.insert(key, v);
            }
        }
        visiting.pop();
        let lookup = |n: &str| resolved.get(n).cloned();
        let r = match eval_const(&c.value, &lookup) {
            Ok(Some(v)) => match uint_max(&c.ty) {
                Some(max) if v > max => Err(format!(
                    "constant '{name}' is {v} ({v:#x}), which does not fit in {} (at most {max:#x})",
                    c.ty
                )),
                Some(_) => Ok(v),
                None => Err(format!(
                    "constant '{name}' has type '{}'; constants are u8, u16, u32 or u64",
                    c.ty
                )),
            },
            Ok(None) => Err(format!(
                "constant '{name}' must be computed from literals and other constants"
            )),
            Err(e) => Err(format!("constant '{name}': {e}")),
        };
        done.insert(name, r.clone());
        r
    }

    let mut done: HashMap<&str, Result<u64, String>> = HashMap::new();
    let mut names: Vec<&str> = decls.keys().copied().collect();
    names.sort();
    let mut values = HashMap::new();
    let mut reported: HashSet<String> = HashSet::new();
    for name in names {
        match eval(name, &decls, &mut done, &mut Vec::new()) {
            Ok(v) => {
                values.insert(name.to_string(), v);
            }
            Err(msg) => {
                // A constant that fails because another one does is
                // reported once, at the one that is wrong.
                if msg.starts_with(&format!("constant '{name}'")) && reported.insert(msg.clone()) {
                    errors.push(err(msg).at(decls[name].span));
                }
            }
        }
    }
    values
}

fn collect_idents<'e>(e: &'e Expr, out: &mut Vec<&'e str>) {
    match e {
        Expr::Ident(n) => out.push(n),
        Expr::Binary { lhs, rhs, .. } => {
            collect_idents(lhs, out);
            collect_idents(rhs, out);
        }
        Expr::BitNot(x) => collect_idents(x, out),
        _ => {}
    }
}

/// A field's width in bits.
fn field_bits(f: &RegField) -> u32 {
    f.hi - f.lo + 1
}

#[derive(Debug, Clone)]
struct VarState {
    /// `None` means this local isn't domain-tracked (e.g. a plain scalar
    /// like `u32`) and is exempt from domain/borrow rules entirely.
    domain: Option<Domain>,
    /// `None` means this local isn't typestate-tracked (either its type
    /// isn't stateful, or it came from a call whose return type isn't
    /// stateful) and is exempt from typestate checks.
    typestate: Option<Typestate>,
    destroyed: bool,
    /// If this local was introduced via `let v = borrow target mode;`,
    /// the target it borrows from and the mode. `None` for ordinary locals.
    borrow_of: Option<(String, BorrowMode)>,
    /// Whether an active `borrow` local has already been `release`d.
    released: bool,
    /// Typestate values are linear: passing one to a parameter that
    /// requires a state (or returning it) consumes it, and the old handle
    /// can never be used again. `Some(how)` once that has happened, where
    /// `how` is a human-readable description for diagnostics.
    consumed: Option<String>,
    /// `Some(..)` for a function parameter: the domain it must be in when
    /// the function returns -- the domain it arrived in, or the `@TO` of a
    /// declared `@FROM -> @TO` change -- and, when a change was declared,
    /// its `@FROM` (for diagnostics).
    param_exit: Option<(Option<Domain>, Option<Domain>)>,
    /// A `let v = borrow ..` whose borrow was rejected (already reported).
    /// Kept as a name so later lines get no "unknown entity" noise, and its
    /// `release` is accepted silently.
    rejected_borrow: bool,
    /// A parameter declared `read`: its contents may only be read, so it
    /// can't be moved, synced, destroyed, write-borrowed, or passed where
    /// a callee may write.
    read_only: bool,
    /// The declared type, where known (used by `swap`).
    ty: Option<TypeExpr>,
    /// Block nesting depth of the declaration (0 = function body).
    block: usize,
    /// For a buffer that is currently split: its parts, and where.
    split_into: Option<(Vec<String>, Span)>,
    /// For a part of a split buffer: the parent and the part's index.
    part_of: Option<(String, usize)>,

    // ---- where things happened, for diagnostics ----------------------
    /// Where this name was declared (for a borrow handle: the borrow).
    origin: Span,
    /// Where the entity entered its current domain, with a description.
    domain_since: Option<(Span, String)>,
    destroyed_at: Option<Span>,
    consumed_at: Option<Span>,
    released_at: Option<Span>,
}

impl VarState {
    fn local(domain: Option<Domain>, typestate: Option<Typestate>, origin: Span) -> Self {
        VarState {
            origin,
            domain_since: None,
            destroyed_at: None,
            consumed_at: None,
            released_at: None,
            domain,
            typestate,
            destroyed: false,
            borrow_of: None,
            released: false,
            consumed: None,
            param_exit: None,
            rejected_borrow: false,
            read_only: false,
            ty: None,
            block: 0,
            split_into: None,
            part_of: None,
        }
    }

    /// Whether this local carries any physical/protocol obligations at all
    /// (as opposed to a plain scalar).
    fn is_tracked(&self) -> bool {
        self.domain.is_some() || self.typestate.is_some() || self.borrow_of.is_some()
    }

    /// Read-only access: a `read` parameter or a read borrow.
    fn is_read_only(&self) -> bool {
        self.read_only || matches!(self.borrow_of, Some((_, BorrowMode::Read)))
    }

    fn is_param(&self) -> bool {
        self.param_exit.is_some()
    }
}

#[derive(Debug, Default, Clone)]
struct BorrowState {
    readers: HashSet<String>,
    writer: Option<String>,
}

impl BorrowState {
    fn is_empty(&self) -> bool {
        self.readers.is_empty() && self.writer.is_none()
    }

    /// Some handle name currently holding this borrow, for diagnostics.
    fn any_holder(&self) -> Option<String> {
        if let Some(w) = &self.writer {
            return Some(w.clone());
        }
        let mut readers: Vec<&String> = self.readers.iter().collect();
        readers.sort();
        readers.first().map(|s| (*s).clone())
    }
}

struct FnChecker<'a> {
    fn_name: String,
    /// `device` register maps (v2.0), by device name.
    devices: &'a HashMap<&'a str, DeviceView<'a>>,
    /// `const` values (v2.4), by name.
    consts: &'a HashMap<String, u64>,
    fn_sigs: &'a HashMap<&'a str, FnSig>,
    state_machines: &'a HashMap<&'a str, HashSet<&'a str>>,
    initial_states: &'a HashMap<&'a str, &'a str>,
    entity_domains: &'a HashMap<&'a str, Option<Domain>>,
    locals: HashMap<String, VarState>,
    /// keyed by the *target* entity name being borrowed from
    borrows: HashMap<String, BorrowState>,
    /// Span of the statement currently being checked.
    cur_span: Span,
    /// Current block nesting depth (0 = function body).
    block_depth: usize,
    /// Set once every path through the statements checked so far has hit a
    /// `return`. Code after that point is unreachable, and the state at that
    /// point must not be merged into the fall-through state of a branch.
    diverged: bool,
    /// Recording each statement's before/after state (for hover).
    trace: Option<Vec<TraceStep>>,
}

/// `move` is only legal along these (from, to) pairs (spec §2.4).
fn legal_move(from: Domain, to: Domain) -> bool {
    matches!(
        (from, to),
        (Domain::Stack, Domain::Ram) | (Domain::Ram, Domain::Dma) | (Domain::Ram, Domain::Device)
    )
}

/// `sync` is only legal starting from these domains; the result is always `@RAM`.
fn legal_sync_source(from: Domain) -> bool {
    matches!(from, Domain::Dma | Domain::Device)
}

/// Domains in which an entity is owned by hardware, not the CPU.
fn is_device_owned(d: Domain) -> bool {
    matches!(d, Domain::Dma | Domain::Device)
}

/// If `arg` can't be passed where `param` is expected, describe what it is
/// instead. Only buffers and entity/typestate base types are compared;
/// scalars convert as in C. `None` arg type (inferred) is not checked here.
fn arg_type_mismatch(param: &TypeExpr, arg: Option<&TypeExpr>) -> Option<String> {
    let arg = arg?;
    match (param, arg) {
        (TypeExpr::Buffer { elem: pe, len: pl }, TypeExpr::Buffer { elem: ae, len: al }) => {
            let len_ok = match (pl, al) {
                (BufferLen::Any, _) => true,
                (BufferLen::Fixed(m), BufferLen::Fixed(n)) => m == n,
                (BufferLen::Fixed(_), BufferLen::Any) => false,
            };
            if pe == ae && len_ok {
                None
            } else if pe == ae {
                Some(match al {
                    BufferLen::Any => format!(
                        "{} (a run-time length, which may not be {})",
                        fmt_type(arg),
                        pl
                    ),
                    BufferLen::Fixed(_) => fmt_type(arg),
                })
            } else {
                Some(fmt_type(arg))
            }
        }
        (TypeExpr::Buffer { .. }, other) => Some(format!("{}, not a buffer", fmt_type(other))),
        (_, TypeExpr::Buffer { .. }) => Some(format!("a buffer ({})", fmt_type(arg))),
        _ => match (param.base_name(), arg.base_name()) {
            (Some(p), Some(a)) if p != a && !is_scalar_name(p) && !is_scalar_name(a) => {
                Some(fmt_type(arg))
            }
            _ => None,
        },
    }
}

fn is_scalar_name(n: &str) -> bool {
    matches!(
        n,
        "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" | "f32" | "f64" | "bool"
    )
}

fn fmt_type(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named(n) => n.clone(),
        TypeExpr::Buffer { elem, len } => format!("buffer<{}, {len}>", fmt_type(elem)),
        TypeExpr::Stateful { name, state } => format!("{name}<{state}>"),
    }
}

fn fmt_state((machine, state): &Typestate) -> String {
    format!("{machine}<{state}>")
}

/// What kind of merge point `merge_branches` is reconciling.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MergeKind {
    If,
    /// Loop back-edge: "ran zero times" vs. "ran once". Requiring the two to
    /// agree means the body leaves every pre-existing entity exactly as it
    /// found it -- a loop invariant, which makes the single pass sound for
    /// any number of iterations.
    Loop,
}

impl<'a> FnChecker<'a> {
    fn new(
        fn_name: &str,
        devices: &'a HashMap<&'a str, DeviceView<'a>>,
        consts: &'a HashMap<String, u64>,
        entity_domains: &'a HashMap<&'a str, Option<Domain>>,
        fn_sigs: &'a HashMap<&'a str, FnSig>,
        state_machines: &'a HashMap<&'a str, HashSet<&'a str>>,
        initial_states: &'a HashMap<&'a str, &'a str>,
    ) -> Self {
        FnChecker {
            fn_name: fn_name.to_string(),
            devices,
            consts,
            entity_domains,
            fn_sigs,
            state_machines,
            initial_states,
            locals: HashMap::new(),
            borrows: HashMap::new(),
            cur_span: Span::default(),
            block_depth: 0,
            diverged: false,
            trace: None,
        }
    }

    /// Every local, described for a reader: its type, domain and anything
    /// else that limits what can be done with it now.
    fn describe_locals(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .locals
            .iter()
            .map(|(name, v)| (name.clone(), self.describe(name, v)))
            .collect();
        out.sort();
        out
    }

    fn describe(&self, name: &str, v: &VarState) -> String {
        if let Some((target, mode)) = &v.borrow_of {
            let mode = match mode {
                BorrowMode::Read => "read",
                BorrowMode::Write => "write",
            };
            return if v.released {
                format!("{mode} borrow of '{target}', released")
            } else {
                format!("{mode} borrow of '{target}'")
            };
        }
        let mut parts: Vec<String> = Vec::new();
        match (&v.typestate, &v.ty) {
            (Some(ts), _) => parts.push(format!("`{}`", fmt_state(ts))),
            (None, Some(t)) => parts.push(format!("`{}`", fmt_type(t))),
            _ => {}
        }
        if v.destroyed {
            parts.push("destroyed".into());
        } else if let Some(how) = &v.consumed {
            parts.push(format!("consumed ({how})"));
        } else {
            if let Some(d) = v.domain {
                let who = match d {
                    Domain::Dma => " (the DMA engine has it)",
                    Domain::Device => " (the device has it)",
                    _ => "",
                };
                parts.push(format!("in @{}{who}", d.name()));
            }
            if let Some((ps, _)) = &v.split_into {
                parts.push(format!("split into {}", ps.join(", ")));
            }
            if let Some((parent, i)) = &v.part_of {
                parts.push(format!("part {i} of '{parent}'"));
            }
            if let Some(b) = self.borrows.get(name) {
                if let Some(w) = &b.writer {
                    parts.push(format!("write-borrowed by '{w}'"));
                }
                if !b.readers.is_empty() {
                    let mut r: Vec<&String> = b.readers.iter().collect();
                    r.sort();
                    let r: Vec<String> = r.iter().map(|x| format!("'{x}'")).collect();
                    parts.push(format!("read-borrowed by {}", r.join(", ")));
                }
            }
            if v.read_only {
                parts.push("`read` parameter".into());
            }
        }
        if parts.is_empty() {
            "a local".into()
        } else {
            parts.join(", ")
        }
    }

    fn check_fn(&mut self, f: &FnDecl, errors: &mut Vec<AnalysisError>) {
        // Parameters are locals too: register them up front (using the
        // domain/typestate already resolved once for this function's
        // signature) so a body that uses a parameter directly -- not just
        // ones re-bound through a `let` -- is checked correctly.
        if let Some(sig) = self.fn_sigs.get(f.name.as_str()) {
            for (i, p) in f.params.iter().enumerate() {
                if let Some(first) = self.locals.get(&p.name) {
                    errors.push(
                        err(format!(
                            "parameter '{}' is declared more than once in fn '{}'",
                            p.name, f.name
                        ))
                        .at(p.span)
                        .about(&p.name)
                        .note(first.origin, "first declared here"),
                    );
                    continue;
                }
                let domain = sig.param_domains.get(i).copied().flatten();
                let mut var =
                    VarState::local(domain, sig.param_states.get(i).cloned().flatten(), p.span);
                var.read_only =
                    sig.param_access.get(i).copied().flatten() == Some(BorrowMode::Read);
                var.ty = Some(p.ty.clone());
                let after = sig.param_domains_after.get(i).copied().flatten();
                var.param_exit = Some((after.or(domain), after.and(domain)));
                if let Some(d) = domain {
                    var.domain_since = Some((
                        p.span,
                        format!("'{}' arrives in @{} here", p.name, d.name()),
                    ));
                }
                self.locals.insert(p.name.clone(), var);
            }
        }

        self.check_block(&f.body, errors);

        // Falling off the end of the body is an implicit `return;`,
        // reported at the closing brace.
        if !self.diverged {
            let before = errors.len();
            self.check_exit_obligations(errors);
            for e in &mut errors[before..] {
                e.span.get_or_insert(f.body.close);
            }
        }
    }

    /// Spec §4.2 (lifetime checker): no entity may escape the function that
    /// owns it while hardware still owns it, no borrow may outlive the
    /// function, and a parameter must be handed back to the caller in the
    /// domain the caller gave it in (the signature is the caller's only view
    /// of it). Checked at every `return` and at the implicit end-of-body one.
    fn check_exit_obligations(&self, errors: &mut Vec<AnalysisError>) {
        let fn_name = &self.fn_name;
        let mut names: Vec<&String> = self.locals.keys().collect();
        names.sort();
        for name in names {
            let var = &self.locals[name];
            if let Some((target, _)) = &var.borrow_of {
                if !var.released {
                    errors.push(
                        err(format!(
                            "borrow '{name}' of '{target}' is still live when '{fn_name}' returns; release it first"
                        ))
                        .note(var.origin, format!("'{name}' borrows '{target}' here")),
                    );
                }
                continue;
            }
            // A split buffer's storage is accounted for by its parts.
            if var.destroyed || var.consumed.is_some() || var.split_into.is_some() {
                continue;
            }
            match (var.param_exit, var.domain) {
                (Some((Some(required), declared_from)), Some(now)) if required != now => {
                    let msg = match declared_from {
                        Some(from) => format!(
                            "parameter '{name}' must be in @{} when '{fn_name}' returns (its signature says @{} -> @{}), but it is in @{}",
                            required.name(),
                            from.name(),
                            required.name(),
                            now.name()
                        ),
                        None => format!(
                            "parameter '{name}' must be back in @{} when '{fn_name}' returns, but it is in @{}",
                            required.name(),
                            now.name()
                        ),
                    };
                    errors.push(err(msg).about(name).note_opt(var.domain_since.as_ref()));
                }
                (Some(_), _) => {}
                (None, Some(now)) if is_device_owned(now) => {
                    errors.push(
                        err(format!(
                            "entity '{name}' is still owned by @{} when '{fn_name}' returns; sync({name}) before it goes out of scope",
                            now.name()
                        ))
                        .about(name)
                        .note_opt(var.domain_since.as_ref()),
                    );
                }
                (None, _) => {}
            }
        }
    }

    fn check_return_value(&mut self, name: &str, errors: &mut Vec<AnalysisError>) {
        let before = errors.len();
        self.check_return_value_inner(name, errors);
        for e in &mut errors[before..] {
            if e.subject.is_none() {
                e.subject = Some(name.to_string());
            }
        }
    }

    fn check_return_value_inner(&mut self, name: &str, errors: &mut Vec<AnalysisError>) {
        let fn_name = self.fn_name.clone();
        let required = self
            .fn_sigs
            .get(fn_name.as_str())
            .and_then(|sig| sig.return_state.clone());
        let Some(var) = self.locals.get(name) else {
            errors.push(err(format!(
                "unknown name '{name}' in return from '{fn_name}'"
            )));
            return;
        };
        if var.destroyed {
            errors.push(
                err(format!("entity '{name}' was destroyed; cannot return it"))
                    .note_opt(Self::lifetime_note(var, name).as_ref()),
            );
            return;
        }
        if let Some(how) = &var.consumed {
            errors.push(
                err(format!(
                    "value '{name}' was already consumed ({how}); cannot return it"
                ))
                .note_opt(Self::lifetime_note(var, name).as_ref()),
            );
            return;
        }
        if var.borrow_of.is_some() {
            errors.push(err(format!(
                "'{name}' is a borrow handle; a borrow cannot be returned from '{fn_name}'"
            )));
            return;
        }
        match (&required, &var.typestate) {
            (Some((req_machine, req_state)), Some((have_machine, have_state))) => {
                if req_machine != have_machine {
                    errors.push(err(format!(
                        "entity '{name}' has typestate '{have_machine}' but function '{fn_name}' must return typestate '{req_machine}'"
                    )));
                } else if req_state != have_state {
                    errors.push(
                        err(format!("expected state {req_state}, found {have_state}")).note(
                            var.origin,
                            format!("'{name}' is {have_machine}<{have_state}> from here"),
                        ),
                    );
                }
            }
            (Some(req), None) => {
                errors.push(err(format!(
                    "'{fn_name}' must return {}, but '{name}' carries no proven typestate",
                    fmt_state(req)
                )));
            }
            _ => {}
        }
        // Returning a tracked value hands it to the caller: it leaves this
        // function's scope, so it no longer counts against exit obligations.
        if var.is_tracked() {
            let span = self.cur_span;
            let v = self.locals.get_mut(name).unwrap();
            v.consumed = Some(format!("returned from '{fn_name}'"));
            v.consumed_at = Some(span);
        }
    }

    fn has_live_borrow(&self, name: &str) -> bool {
        self.borrows
            .get(name)
            .map(|b| !b.is_empty())
            .unwrap_or(false)
    }

    /// Shared preconditions for `move` / `sync` / `destroy`: the name must be
    /// a live, owned entity (not a borrow handle, not destroyed, not
    /// consumed) with no outstanding borrow. Returns its state on success.
    /// `swap a, b;` and `rotate a, b, c, ..;` -- every name must be a live,
    /// unborrowed, owned local buffer, all of the same type and declared in
    /// the same block. Each name then takes the next one's storage (the last
    /// takes the first's), together with everything the checker knows about
    /// it (domain, history). `swap a, b` is the two-name rotation.
    fn check_permute(&mut self, names: &[String], verb: &str, errors: &mut Vec<AnalysisError>) {
        let stmt = format!("{verb} {}", names.join(", "));
        if names.len() < 2 {
            errors.push(err(format!("`{verb}` needs at least two buffers")));
            return;
        }
        for (i, a) in names.iter().enumerate() {
            if names[..i].contains(a) {
                errors.push(err(format!("cannot {verb} '{a}' with itself")).about(a));
                return;
            }
        }
        let mut vars = Vec::new();
        for n in names {
            vars.push(self.owned_target(n, verb, errors));
        }
        let Some(vars) = vars.into_iter().collect::<Option<Vec<VarState>>>() else {
            return;
        };
        let what = if verb == "swap" { "swapped" } else { "rotated" };
        for (name, v) in names.iter().zip(&vars) {
            if v.is_param() {
                errors.push(
                    err(format!(
                        "cannot {verb} parameter '{name}': its storage belongs to the caller"
                    ))
                    .about(name)
                    .note(
                        v.origin,
                        format!("'{name}' is a parameter of '{}'", self.fn_name),
                    ),
                );
                return;
            }
            if !matches!(v.ty, Some(TypeExpr::Buffer { .. })) {
                errors.push(
                    err(format!(
                        "'{name}' is not a buffer; only buffers can be {what}"
                    ))
                    .about(name)
                    .note(v.origin, format!("'{name}' is declared here")),
                );
                return;
            }
        }
        let (a, va) = (&names[0], &vars[0]);
        for (b, vb) in names.iter().zip(&vars).skip(1) {
            // Storage moves between names, so all storages must live exactly
            // as long: a name declared outside a block must not end up
            // pointing at storage that dies with the block.
            if va.block != vb.block {
                let (inner, outer, iv) = if va.block > vb.block {
                    (a, b, va)
                } else {
                    (b, a, vb)
                };
                errors.push(
                    err(format!(
                        "cannot {verb} '{a}' with '{b}': '{inner}' is declared in an inner block, so '{outer}' would be left pointing at storage that dies when that block ends"
                    ))
                    .about(inner)
                    .note(iv.origin, format!("'{inner}' is declared here")),
                );
                return;
            }
            if va.ty != vb.ty {
                let show = |t: &Option<TypeExpr>| t.as_ref().map(fmt_type).unwrap_or_default();
                errors.push(
                    err(format!(
                        "cannot {verb} '{a}' ({}) with '{b}' ({}): the buffer types differ",
                        show(&va.ty),
                        show(&vb.ty)
                    ))
                    .about(a),
                );
                return;
            }
        }
        let span = self.cur_span;
        let n = names.len();
        for (i, name) in names.iter().enumerate() {
            let mut v = vars[(i + 1) % n].clone();
            if let Some(d) = v.domain {
                v.domain_since = Some((
                    span,
                    format!("'{name}' is @{} after `{stmt}` here", d.name()),
                ));
            }
            self.locals.insert(name.clone(), v);
        }
    }

    /// `split parent into p0, p1, ..;` -- the parent must be a live,
    /// unborrowed, owned local buffer whose fixed length divides evenly.
    /// Each part is a new buffer of `len / n` in the parent's domain; the
    /// parent is unusable until the parts are joined.
    fn check_split(&mut self, parent: &str, parts: &[String], errors: &mut Vec<AnalysisError>) {
        let Some(pv) = self.owned_target(parent, "split", errors) else {
            return;
        };
        if pv.is_param() {
            errors.push(
                err(format!(
                    "cannot split parameter '{parent}': its storage belongs to the caller"
                ))
                .about(parent),
            );
            return;
        }
        let n = parts.len();
        let (elem, len) = match &pv.ty {
            Some(TypeExpr::Buffer {
                elem,
                len: BufferLen::Fixed(len),
            }) => (elem.clone(), *len),
            _ => {
                errors.push(
                    err(format!(
                        "'{parent}' cannot be split: only buffers with a fixed length can be"
                    ))
                    .about(parent)
                    .note(pv.origin, format!("'{parent}' is declared here")),
                );
                return;
            }
        };
        if n < 2 || len % n as u64 != 0 {
            errors.push(
                err(format!(
                    "cannot split '{parent}' (buffer<{}, {len}>) into {n} equal parts",
                    fmt_type(&elem)
                ))
                .about(parent),
            );
            return;
        }
        for (i, part) in parts.iter().enumerate() {
            if parts[..i].contains(part) || part == parent {
                errors.push(err(format!("'{part}' is named twice in this split")).about(part));
                return;
            }
            if !self.check_no_shadowing(part, errors) {
                return;
            }
        }
        let span = self.cur_span;
        let part_ty = TypeExpr::Buffer {
            elem,
            len: BufferLen::Fixed(len / n as u64),
        };
        for (i, part) in parts.iter().enumerate() {
            let mut v = VarState::local(pv.domain, None, span);
            v.ty = Some(part_ty.clone());
            v.block = self.block_depth;
            v.part_of = Some((parent.to_string(), i));
            if let Some(d) = pv.domain {
                v.domain_since = Some((
                    span,
                    format!(
                        "'{part}' is part {i} of '{parent}', in @{}, from here",
                        d.name()
                    ),
                ));
            }
            self.locals.insert(part.clone(), v);
        }
        let p = self.locals.get_mut(parent).unwrap();
        p.split_into = Some((parts.to_vec(), span));
    }

    /// `join p0, p1, .. into parent;` -- exactly the parts `parent` was
    /// split into, in order (a part that was swapped away no longer counts),
    /// each live, unborrowed and owned, all in one domain -- which the
    /// parent then has.
    fn check_join(&mut self, parent: &str, parts: &[String], errors: &mut Vec<AnalysisError>) {
        let Some(pv) = self.locals.get(parent).cloned() else {
            errors.push(err(format!("unknown entity '{parent}'")).about(parent));
            return;
        };
        let Some((expected, split_at)) = pv.split_into.clone() else {
            errors.push(err(format!("cannot join into '{parent}': it is not split")).about(parent));
            return;
        };
        if parts != expected.as_slice() {
            errors.push(
                err(format!(
                    "'{parent}' was split into {}; join exactly those parts, in that order",
                    expected.join(", ")
                ))
                .about(parent)
                .note(split_at, format!("'{parent}' is split here")),
            );
            return;
        }
        let mut domain = None;
        let mut vars = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let Some(v) = self.owned_target(part, "join", errors) else {
                return;
            };
            if v.part_of != Some((parent.to_string(), i)) {
                errors.push(
                    err(format!(
                        "'{part}' no longer holds part {i} of '{parent}' (it was swapped or rotated); join the parts in their original places"
                    ))
                    .about(part)
                    .note_opt(v.domain_since.as_ref()),
                );
                return;
            }
            vars.push(v);
        }
        for (part, v) in parts.iter().zip(&vars) {
            match (domain, v.domain) {
                (None, d) => domain = d,
                (Some(d), Some(e)) if d != e => {
                    let mut e = err(format!(
                        "cannot join '{}' (@{}) and '{part}' (@{}) into '{parent}': its parts must all be in the same domain",
                        parts[0],
                        d.name(),
                        e.name()
                    ))
                    .about(part)
                    .note_opt(v.domain_since.as_ref());
                    e = e.note_opt(vars[0].domain_since.as_ref());
                    errors.push(e);
                    return;
                }
                _ => {}
            }
        }
        let span = self.cur_span;
        for part in parts {
            let v = self.locals.get_mut(part).unwrap();
            v.consumed = Some(format!("joined back into '{parent}'"));
            v.consumed_at = Some(span);
        }
        let p = self.locals.get_mut(parent).unwrap();
        p.split_into = None;
        p.domain = domain;
        if let Some(d) = domain {
            p.domain_since = Some((
                span,
                format!("'{parent}' is joined back in @{} here", d.name()),
            ));
        }
    }

    /// If `name` is a split buffer, the error for using it as a whole.
    fn split_error(var: &VarState, name: &str, what: &str) -> Option<AnalysisError> {
        let (parts, at) = var.split_into.as_ref()?;
        Some(
            err(format!(
                "'{name}' is split into {}; join them back first (cannot {what})",
                parts.join(", ")
            ))
            .about(name)
            .note(*at, format!("'{name}' is split here")),
        )
    }

    /// A note pointing at a live borrow of `target`, if there is one.
    fn borrow_note(&self, target: &str) -> Option<(Span, String)> {
        let holder = self.borrows.get(target)?.any_holder()?;
        let origin = self.locals.get(&holder)?.origin;
        Some((origin, format!("'{target}' is borrowed by '{holder}' here")))
    }

    /// Notes explaining why `var` is no longer usable (destroyed, consumed,
    /// released), if any.
    fn lifetime_note(var: &VarState, name: &str) -> Option<(Span, String)> {
        if let Some(at) = var.destroyed_at {
            return Some((at, format!("'{name}' is destroyed here")));
        }
        if let Some(at) = var.consumed_at {
            return Some((at, format!("'{name}' is consumed here")));
        }
        if let Some(at) = var.released_at {
            return Some((at, format!("'{name}' is released here")));
        }
        None
    }

    /// `sync(name)`, or `wait event for name`: the hardware gives `name`
    /// back to the CPU.
    fn hand_back(&mut self, name: &str, event: Option<&str>, errors: &mut Vec<AnalysisError>) {
        let what = match event {
            None => "sync".to_string(),
            Some(e) => format!("wait {e} for"),
        };
        let Some(var) = self.owned_target(name, &what, errors) else {
            return;
        };
        let Some(from) = var.domain else { return };
        if !legal_sync_source(from) {
            let msg = match event {
                None => format!(
                    "cannot sync entity '{name}': no legal sync transition from @{}",
                    from.name()
                ),
                Some(e) => format!(
                    "`wait {e} for {name}`, but '{name}' is in @{}: the hardware doesn't have it, so there is nothing for `{e}` to give back",
                    from.name()
                ),
            };
            let mut e = err(msg).about(name);
            if event.is_some() {
                e = e.note_opt(var.domain_since.as_ref());
            }
            errors.push(e);
            return;
        }
        let span = self.cur_span;
        let v = self.locals.get_mut(name).unwrap();
        v.domain = Some(Domain::Ram);
        v.domain_since = Some((
            span,
            match event {
                None => format!("'{name}' is synced back to @RAM here"),
                Some(e) => format!("'{name}' is handed back to @RAM by `wait {e}` here"),
            },
        ));
    }

    fn owned_target(
        &self,
        name: &str,
        verb: &str,
        errors: &mut Vec<AnalysisError>,
    ) -> Option<VarState> {
        let Some(var) = self.locals.get(name) else {
            errors.push(err(format!("unknown entity '{name}'")).about(name));
            return None;
        };
        if let Some((target, _)) = &var.borrow_of {
            errors.push(
                err(format!(
                    "'{name}' is a borrow of '{target}'; cannot {verb} a borrow handle (release it, then {verb} '{target}')"
                ))
                .about(name)
                .note(var.origin, format!("'{name}' borrows '{target}' here")),
            );
            return None;
        }
        if var.read_only {
            errors.push(
                err(format!(
                    "'{name}' is a `read` parameter of '{}'; cannot {verb} it",
                    self.fn_name
                ))
                .about(name)
                .note(var.origin, format!("'{name}' is declared `read` here")),
            );
            return None;
        }
        if var.destroyed {
            let msg = if verb == "destroy" {
                format!("entity '{name}' was already destroyed")
            } else {
                format!("entity '{name}' was destroyed; cannot {verb}")
            };
            errors.push(
                err(msg)
                    .about(name)
                    .note_opt(Self::lifetime_note(var, name).as_ref()),
            );
            return None;
        }
        if let Some(how) = &var.consumed {
            errors.push(
                err(format!(
                    "value '{name}' was already consumed ({how}); cannot {verb}"
                ))
                .about(name)
                .note_opt(Self::lifetime_note(var, name).as_ref()),
            );
            return None;
        }
        if let Some(e) = Self::split_error(var, name, &format!("{verb} it")) {
            errors.push(e);
            return None;
        }
        if self.has_live_borrow(name) {
            errors.push(
                err(format!("entity '{name}' has a live borrow; cannot {verb}"))
                    .about(name)
                    .note_opt(self.borrow_note(name).as_ref()),
            );
            return None;
        }
        Some(var.clone())
    }

    fn check_stmt(&mut self, stmt: &Stmt, errors: &mut Vec<AnalysisError>) {
        match stmt {
            Stmt::VarDecl {
                name,
                ty,
                domain,
                init,
            } => self.check_var_decl(name, ty.as_ref(), *domain, init.as_ref(), errors),

            Stmt::Move { name, to } => {
                let Some(var) = self.owned_target(name, "move", errors) else {
                    return;
                };
                let Some(from) = var.domain else { return };
                if !legal_move(from, *to) {
                    errors.push(err(format!(
                        "no legal transition from @{} to @{} via move",
                        from.name(),
                        to.name()
                    )));
                    return;
                }
                let span = self.cur_span;
                let v = self.locals.get_mut(name).unwrap();
                v.domain = Some(*to);
                v.domain_since = Some((span, format!("'{name}' is moved to @{} here", to.name())));
            }

            Stmt::Sync { name } => self.hand_back(name, None, errors),
            // v2.3: `wait E for b` -- the interrupt that signals E says the
            // hardware is done with `b`: a hand-back, like `sync`. (The
            // event rules are checked with the other interrupt rules.)
            Stmt::Wait {
                name: event,
                buf: Some(name),
            } => self.hand_back(name, Some(event), errors),

            Stmt::Release { name } => {
                let Some(var) = self.locals.get(name) else {
                    errors.push(err(format!("unknown entity '{name}'")));
                    return;
                };
                if var.rejected_borrow {
                    return; // the borrow itself was already reported
                }
                let Some((target, mode)) = var.borrow_of.clone() else {
                    errors.push(err(format!("'{name}' is not an active borrow")));
                    return;
                };
                if var.released {
                    errors.push(
                        err(format!("borrow '{name}' was already released"))
                            .about(name)
                            .note_opt(Self::lifetime_note(var, name).as_ref()),
                    );
                    return;
                }
                if let Some(bs) = self.borrows.get_mut(&target) {
                    match mode {
                        BorrowMode::Read => {
                            bs.readers.remove(name);
                        }
                        BorrowMode::Write => {
                            if bs.writer.as_deref() == Some(name.as_str()) {
                                bs.writer = None;
                            }
                        }
                    }
                }
                let span = self.cur_span;
                let v = self.locals.get_mut(name).unwrap();
                v.released = true;
                v.released_at = Some(span);
            }

            Stmt::Swap { a, b } => self.check_permute(&[a.clone(), b.clone()], "swap", errors),
            Stmt::Rotate { names } => self.check_permute(names, "rotate", errors),
            Stmt::Split { parent, parts } => self.check_split(parent, parts, errors),
            Stmt::Join { parent, parts } => self.check_join(parent, parts, errors),

            Stmt::Destroy { name } => {
                let Some(var) = self.owned_target(name, "destroy", errors) else {
                    return;
                };
                if let Some((parent, _)) = &var.part_of {
                    errors.push(
                        err(format!(
                            "cannot destroy '{name}' on its own: it is part of '{parent}' (join the parts back, then destroy '{parent}')"
                        ))
                        .about(name),
                    );
                    return;
                }
                if var.is_param() {
                    errors.push(
                        err(format!(
                            "cannot destroy parameter '{name}': the caller still owns it"
                        ))
                        .about(name)
                        .note(
                            var.origin,
                            format!("'{name}' is a parameter of '{}'", self.fn_name),
                        ),
                    );
                    return;
                }
                if let Some(d) = var.domain.filter(|d| is_device_owned(*d)) {
                    errors.push(
                        err(format!(
                            "entity '{name}' is still owned by @{}; sync({name}) before destroying it",
                            d.name()
                        ))
                        .about(name)
                        .note_opt(var.domain_since.as_ref()),
                    );
                    return;
                }
                let span = self.cur_span;
                let v = self.locals.get_mut(name).unwrap();
                v.destroyed = true;
                v.destroyed_at = Some(span);
            }

            Stmt::Expr(e) => self.check_expr(e, errors),

            Stmt::Return(value) => {
                match value {
                    Some(Expr::Ident(name)) => self.check_return_value(name, errors),
                    Some(e) => {
                        self.check_expr(e, errors);
                        let fn_name = self.fn_name.clone();
                        if let Some(req) = self
                            .fn_sigs
                            .get(fn_name.as_str())
                            .and_then(|s| s.return_state.clone())
                        {
                            errors.push(err(format!(
                                "'{fn_name}' must return {}, which can only be returned by name",
                                fmt_state(&req)
                            )));
                        }
                    }
                    None => {}
                }
                self.check_exit_obligations(errors);
                self.diverged = true;
            }

            Stmt::If {
                cond,
                then_block,
                else_block,
            } => self.check_if(cond, then_block, else_block.as_ref(), errors),

            Stmt::While { cond, body } => self.check_while(cond, body, errors),

            // M9: `name = expr;`. Deliberately scoped to plain scalar
            // locals -- domain-tracked entities/buffers already have their
            // own dedicated transitions (`move`/`sync`/`borrow`/`destroy`)
            // and reassigning through them would bypass those checks, so
            // it's a clear error rather than a silent domain reset.
            // v2.2: events are checked with the other interrupt rules.
            Stmt::Signal { .. } | Stmt::Wait { buf: None, .. } => {}
            // Runs straight through (a `return` inside is an error).
            Stmt::Critical { body } => {
                let base_locals = self.locals.clone();
                let base_borrows = self.borrows.clone();
                let (locals, borrows, diverged) = self.run_branch(Some(body), errors);
                if diverged {
                    self.diverged = true;
                } else {
                    self.adopt_branch(&base_locals, &base_borrows, locals, borrows, errors);
                }
            }
            Stmt::Assign { name, value } => {
                let Some(var) = self.locals.get(name) else {
                    errors.push(err(format!("cannot assign to unknown local '{name}'")));
                    self.check_expr(value, errors);
                    return;
                };
                if var.borrow_of.is_some() {
                    errors.push(err(format!(
                        "cannot assign to '{name}': it is a borrow handle"
                    )));
                } else if var.domain.is_some() || var.typestate.is_some() {
                    errors.push(err(format!(
                        "cannot assign to '{name}': it is domain-tracked (use move/sync instead)"
                    )));
                } else if var.destroyed {
                    errors.push(err(format!("cannot assign to '{name}': it was destroyed")));
                }
                self.check_expr(value, errors);
            }
        }
    }

    fn check_block(&mut self, block: &phase_ast::Block, errors: &mut Vec<AnalysisError>) {
        for stmt in &block.stmts {
            if self.diverged {
                errors.push(
                    err(format!(
                        "unreachable statement after `return` in fn '{}'",
                        self.fn_name
                    ))
                    .at(stmt.span),
                );
                return;
            }
            let before = errors.len();
            self.cur_span = stmt.span;
            let snapshot = self.trace.as_ref().map(|_| self.describe_locals());
            self.check_stmt(stmt, errors);
            if let Some(before_state) = snapshot {
                let step = TraceStep {
                    fn_name: self.fn_name.clone(),
                    span: stmt.span,
                    compound: matches!(
                        stmt.node,
                        Stmt::If { .. } | Stmt::While { .. } | Stmt::Critical { .. }
                    ),
                    before: before_state,
                    after: self.describe_locals(),
                };
                if let Some(t) = self.trace.as_mut() {
                    t.push(step);
                }
            }
            // Errors raised while checking this statement (and not already
            // pinned to a nested statement) point at it.
            for e in &mut errors[before..] {
                e.span.get_or_insert(stmt.span);
            }
        }
    }

    /// Check `block` starting from the current state, then restore the
    /// current state. Returns the state the block ended in, and whether the
    /// block diverged (every path through it returned).
    fn run_branch(
        &mut self,
        block: Option<&phase_ast::Block>,
        errors: &mut Vec<AnalysisError>,
    ) -> (
        HashMap<String, VarState>,
        HashMap<String, BorrowState>,
        bool,
    ) {
        let saved_locals = self.locals.clone();
        let saved_borrows = self.borrows.clone();
        let saved_diverged = self.diverged;
        if let Some(b) = block {
            self.block_depth += 1;
            self.check_block(b, errors);
            self.block_depth -= 1;
            if !self.diverged {
                self.check_block_scope_exit(&saved_locals, b.close, errors);
            }
        }
        let out_locals = std::mem::replace(&mut self.locals, saved_locals);
        let out_borrows = std::mem::replace(&mut self.borrows, saved_borrows);
        let out_diverged = std::mem::replace(&mut self.diverged, saved_diverged);
        (out_locals, out_borrows, out_diverged)
    }

    /// Names declared inside a block go out of scope when it falls through
    /// to its closing brace: none may still be owned by hardware (the
    /// storage is gone while a transfer may still target it). Borrows taken
    /// in the block are checked at the join. Reported at the `}`.
    fn check_block_scope_exit(
        &self,
        outer: &HashMap<String, VarState>,
        close: Span,
        errors: &mut Vec<AnalysisError>,
    ) {
        let mut names: Vec<&String> = self
            .locals
            .keys()
            .filter(|n| !outer.contains_key(*n))
            .collect();
        names.sort();
        for name in names {
            let var = &self.locals[name];
            if var.borrow_of.is_some()
                || var.destroyed
                || var.consumed.is_some()
                || var.split_into.is_some()
            {
                continue;
            }
            if let Some(d) = var.domain.filter(|d| is_device_owned(*d)) {
                errors.push(
                    err(format!(
                        "entity '{name}' is still owned by @{} when its block ends; sync({name}) before it goes out of scope",
                        d.name()
                    ))
                    .at(close)
                    .about(name)
                    .note_opt(var.domain_since.as_ref()),
                );
            }
        }
    }

    /// Adopt one branch's end state wholesale (the other branch returned,
    /// so it never reaches the join point), dropping names that were
    /// scoped to the branch.
    fn adopt_branch(
        &mut self,
        base_locals: &HashMap<String, VarState>,
        base_borrows: &HashMap<String, BorrowState>,
        locals: HashMap<String, VarState>,
        borrows: HashMap<String, BorrowState>,
        errors: &mut Vec<AnalysisError>,
    ) {
        self.report_branch_local_borrows(base_borrows, [&borrows], errors);
        self.locals = locals
            .into_iter()
            .filter(|(k, _)| base_locals.contains_key(k))
            .collect();
        self.borrows = borrows
            .into_iter()
            .filter(|(k, _)| base_borrows.contains_key(k))
            .collect();
    }

    /// M4 — `ENTITY_PHI`: check each branch starting from the *same*
    /// pre-branch state, then merge. A variable that already existed
    /// before the `if` must agree on domain/typestate/destroyed-ness on
    /// every incoming path, or the merge is rejected (bug-gallery #6).
    /// A variable introduced *inside* a branch is properly out of scope
    /// afterward — it's simply not carried into the merged state, since
    /// only pre-existing names are merged. A branch that ends in `return`
    /// never reaches the join point, so it does not take part in the merge.
    fn check_if(
        &mut self,
        cond: &Expr,
        then_block: &phase_ast::Block,
        else_block: Option<&phase_ast::Block>,
        errors: &mut Vec<AnalysisError>,
    ) {
        self.check_expr(cond, errors);

        let base_locals = self.locals.clone();
        let base_borrows = self.borrows.clone();

        let (then_locals, then_borrows, then_div) = self.run_branch(Some(then_block), errors);
        let (else_locals, else_borrows, else_div) = self.run_branch(else_block, errors);

        match (then_div, else_div) {
            (true, true) => self.diverged = true,
            (true, false) => self.adopt_branch(
                &base_locals,
                &base_borrows,
                else_locals,
                else_borrows,
                errors,
            ),
            (false, true) => self.adopt_branch(
                &base_locals,
                &base_borrows,
                then_locals,
                then_borrows,
                errors,
            ),
            (false, false) => self.merge_branches(
                MergeKind::If,
                &base_locals,
                &base_borrows,
                &then_locals,
                &then_borrows,
                &else_locals,
                &else_borrows,
                errors,
            ),
        }
    }

    /// M4 — a `while` loop is checked as "body executed once" merged
    /// against "body executed zero times" (the pre-loop state itself).
    /// Because the merge requires both to agree on every pre-existing
    /// entity, the body must leave the state exactly as it found it — a
    /// loop invariant — so the single pass is a genuine fixed point and the
    /// result holds for any number of iterations.
    fn check_while(
        &mut self,
        cond: &Expr,
        body: &phase_ast::Block,
        errors: &mut Vec<AnalysisError>,
    ) {
        self.check_expr(cond, errors);

        let base_locals = self.locals.clone();
        let base_borrows = self.borrows.clone();

        let (body_locals, body_borrows, body_div) = self.run_branch(Some(body), errors);

        if body_div {
            // Every iteration returns: falling out of the loop is only
            // possible when the body never ran, i.e. the pre-loop state.
            self.report_branch_local_borrows(&base_borrows, [&body_borrows], errors);
            return;
        }

        self.merge_branches(
            MergeKind::Loop,
            &base_locals,
            &base_borrows,
            &body_locals,
            &body_borrows,
            &base_locals,
            &base_borrows,
            errors,
        );
    }

    /// A borrow first taken *inside* a branch (its target wasn't borrowed
    /// before the branch) must be fully released before that branch ends
    /// -- its handle variable is scoped to the branch and has no way to be
    /// released afterward, so an outstanding borrow here can never be
    /// released at all.
    fn report_branch_local_borrows<'b>(
        &self,
        base_borrows: &HashMap<String, BorrowState>,
        branches: impl IntoIterator<Item = &'b HashMap<String, BorrowState>>,
        errors: &mut Vec<AnalysisError>,
    ) {
        let mut already_reported = HashSet::new();
        let mut offenders: Vec<String> = Vec::new();
        for borrows in branches {
            for (target, bs) in borrows {
                if !base_borrows.contains_key(target)
                    && !bs.is_empty()
                    && already_reported.insert(target.clone())
                {
                    offenders.push(target.clone());
                }
            }
        }
        offenders.sort();
        for target in offenders {
            errors.push(err(format!(
                "borrow of '{target}' taken inside a branch must be released before the branch ends"
            )));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_branches(
        &mut self,
        kind: MergeKind,
        base_locals: &HashMap<String, VarState>,
        base_borrows: &HashMap<String, BorrowState>,
        a_locals: &HashMap<String, VarState>,
        a_borrows: &HashMap<String, BorrowState>,
        b_locals: &HashMap<String, VarState>,
        b_borrows: &HashMap<String, BorrowState>,
        errors: &mut Vec<AnalysisError>,
    ) {
        // Only names that existed *before* the branch are merged -- a name
        // introduced inside a branch is out of scope once it ends, on
        // either path, so it's simply dropped rather than merged.
        let loop_note = if kind == MergeKind::Loop {
            "; a `while` body must leave every entity exactly as it found it"
        } else {
            ""
        };
        let mut names: Vec<&String> = base_locals.keys().collect();
        names.sort();
        let mut merged_locals = HashMap::new();
        for name in names {
            let base_var = &base_locals[name];
            let a = a_locals
                .get(name)
                .expect("pre-existing local must survive both branches");
            let b = b_locals
                .get(name)
                .expect("pre-existing local must survive both branches");

            // `None` on one side while the other is known means an earlier
            // error already poisoned that path; don't report it again.
            let domain = if a.domain == b.domain {
                a.domain
            } else if a.domain.is_none() || b.domain.is_none() {
                None
            } else {
                let (da, db) = (
                    a.domain.map(|d| d.name()).unwrap_or("?"),
                    b.domain.map(|d| d.name()).unwrap_or("?"),
                );
                // For a loop, `a` is the end of the body and `b` the state
                // the loop was entered with.
                let msg = match kind {
                    MergeKind::Loop => format!(
                        "entity '{name}' is @{da} at the end of the `while` body but @{db} when the loop is entered{loop_note}"
                    ),
                    MergeKind::If => format!(
                        "entity '{name}' disagrees on domain across incoming branches (@{da} vs @{db})"
                    ),
                };
                let mut e = err(msg).about(name).note_opt(a.domain_since.as_ref());
                if b.domain_since.as_ref().map(|n| n.0) != a.domain_since.as_ref().map(|n| n.0) {
                    e = e.note_opt(b.domain_since.as_ref());
                }
                errors.push(e);
                None
            };

            let typestate = if a.typestate == b.typestate {
                a.typestate.clone()
            } else if a.typestate.is_none() || b.typestate.is_none() {
                None
            } else {
                errors.push(err(format!(
                    "entity '{name}' disagrees on typestate across incoming branches{loop_note}"
                )));
                None
            };

            let destroyed = if a.destroyed == b.destroyed {
                a.destroyed
            } else {
                errors.push(err(format!(
                    "entity '{name}' disagrees on destroyed-state across incoming branches (destroyed on one path but not the other){loop_note}"
                )));
                true // conservative: treat as destroyed so further use is blocked
            };

            let consumed = match (&a.consumed, &b.consumed) {
                (x, y) if x == y => x.clone(),
                _ if kind == MergeKind::Loop => {
                    errors.push(err(format!(
                        "value '{name}' is consumed inside a `while` loop but was created before it; the next iteration would use it again"
                    )));
                    Some("inside a `while` loop".to_string())
                }
                // Consumed on one path only: like Rust's "maybe moved", it is
                // fine as long as nothing touches it afterward.
                (Some(how), None) | (None, Some(how)) => {
                    Some(format!("{how}, on one branch of an `if`"))
                }
                (Some(how), Some(_)) => Some(how.clone()),
                (None, None) => unreachable!(),
            };

            // Split state and part identity must agree too: `join` relies on
            // both, and a loop body must leave them as it found them.
            let split_of = |v: &VarState| v.split_into.as_ref().map(|(p, _)| p.clone());
            if split_of(a) != split_of(b) {
                errors.push(
                    err(format!(
                        "entity '{name}' is split on one path but not the other{loop_note}"
                    ))
                    .about(name)
                    .note_opt(
                        a.split_into
                            .as_ref()
                            .or(b.split_into.as_ref())
                            .map(|(_, at)| (*at, format!("'{name}' is split here")))
                            .as_ref(),
                    ),
                );
            }
            if a.part_of != b.part_of {
                errors.push(
                    err(format!(
                        "entity '{name}' holds a different part of a split buffer on each path{loop_note}"
                    ))
                    .about(name),
                );
            }

            merged_locals.insert(
                name.clone(),
                VarState {
                    domain,
                    typestate,
                    destroyed,
                    borrow_of: base_var.borrow_of.clone(),
                    released: a.released || b.released,
                    consumed,
                    param_exit: base_var.param_exit,
                    read_only: base_var.read_only,
                    ty: base_var.ty.clone(),
                    block: base_var.block,
                    split_into: a.split_into.clone(),
                    part_of: a.part_of.clone(),
                    rejected_borrow: base_var.rejected_borrow,
                    origin: base_var.origin,
                    domain_since: a.domain_since.clone().or_else(|| b.domain_since.clone()),
                    destroyed_at: a.destroyed_at.or(b.destroyed_at),
                    consumed_at: a.consumed_at.or(b.consumed_at),
                    released_at: a.released_at.or(b.released_at),
                },
            );
        }
        self.locals = merged_locals;

        // Borrow targets that already existed before the branch must agree
        // on borrow state across both paths (e.g. released on one path but
        // not the other is a disagreement, same as any other property).
        let empty = BorrowState::default();
        let mut merged_borrows = HashMap::new();
        let mut targets: Vec<&String> = base_borrows.keys().collect();
        targets.sort();
        for target in targets {
            let a_bs = a_borrows.get(target).unwrap_or(&empty);
            let b_bs = b_borrows.get(target).unwrap_or(&empty);
            let agrees = a_bs.readers == b_bs.readers && a_bs.writer == b_bs.writer;
            if !agrees {
                errors.push(err(format!(
                    "entity '{target}' disagrees on borrow state across incoming branches{loop_note}"
                )));
            }
            let chosen = if !a_bs.is_empty() {
                a_bs.clone()
            } else {
                b_bs.clone()
            };
            merged_borrows.insert(target.clone(), chosen);
        }
        self.borrows = merged_borrows;

        self.report_branch_local_borrows(base_borrows, [a_borrows, b_borrows], errors);
    }

    fn check_no_shadowing(&self, name: &str, errors: &mut Vec<AnalysisError>) -> bool {
        if self.consts.contains_key(name) {
            errors.push(
                err(format!(
                    "'{name}' is a constant; choose another name for this local"
                ))
                .about(name),
            );
            return false;
        }
        if let Some(d) = self.devices.get(name) {
            errors.push(
                err(format!(
                    "'{name}' is a device; choose another name for this local"
                ))
                .about(name)
                .note(d.span, format!("device '{name}' is declared here")),
            );
            return false;
        }
        if let Some(existing) = self.locals.get(name) {
            errors.push(
                err(format!(
                    "'{name}' is already declared in fn '{}'; PHASE does not allow shadowing or redeclaration",
                    self.fn_name
                ))
                .about(name)
                .note(existing.origin, format!("'{name}' is first declared here")),
            );
            return false;
        }
        true
    }

    fn check_var_decl(
        &mut self,
        name: &str,
        ty: Option<&TypeExpr>,
        domain: Option<Domain>,
        init: Option<&Expr>,
        errors: &mut Vec<AnalysisError>,
    ) {
        if !self.check_no_shadowing(name, errors) {
            // Still check the initializer so its own errors are reported,
            // but leave the existing binding untouched.
            if let Some(e) = init {
                if !matches!(e, Expr::Borrow { .. }) {
                    self.check_expr(e, errors);
                }
            }
            return;
        }

        // `let v = borrow target mode;` — special-cased: `v` is not itself
        // a fresh domain entity, it's a handle onto `target`'s borrow state.
        if let Some(Expr::Borrow { target, mode }) = init {
            self.check_borrow_decl(name, target, *mode, errors);
            return;
        }

        // `let y = x;` where `x` is an entity: entities are not copyable,
        // and silently aliasing one would give the same storage two
        // independently-tracked names (each could be moved/synced/destroyed
        // on its own). Rejected outright; `borrow` is the way to get a
        // second name for an entity.
        if let Some(Expr::Ident(src)) = init {
            if let Some(src_var) = self.locals.get(src) {
                if src_var.is_tracked() {
                    errors.push(
                        err(format!(
                            "cannot bind '{name}' to entity '{src}': entities are not copyable (use `borrow {src} read|write` for a second name)"
                        ))
                        .about(src),
                    );
                    return;
                }
            }
        }

        if let Some(TypeExpr::Buffer {
            len: BufferLen::Any,
            ..
        }) = ty
        {
            errors.push(
                err(format!(
                    "'{name}' needs a concrete length: `_` is only allowed in parameter types, where the length is passed at run time"
                ))
                .about(name),
            );
        }

        let resolved_domain = match (domain, ty) {
            (Some(d), _) => Some(d),
            (None, Some(TypeExpr::Buffer { .. })) => {
                errors.push(err(format!(
                    "entity '{name}' has no domain: buffer types require an explicit @domain"
                )));
                None
            }
            (None, Some(t)) => t
                .base_name()
                .and_then(|n| self.entity_domains.get(n))
                .copied()
                .flatten(),
            (None, None) => None,
        };

        // Check the initializer *before* the new name is in scope, so a
        // call that consumes a typestate value is accounted for first.
        if let Some(e) = init {
            self.check_expr(e, errors);
        }

        // Typestate. A state can only be *established* by something that
        // proves it: the declared return state of the function being
        // called, or -- for a freshly-constructed value -- the machine's
        // initial state. An annotation is a claim to be checked against
        // that evidence, never a source of it.
        let declared = ty.and_then(|t| resolve_stateful(t, self.state_machines, errors));
        let call_state = match init {
            Some(Expr::Call { callee, .. }) => Some(
                self.fn_sigs
                    .get(callee.as_str())
                    .and_then(|sig| sig.return_state.clone()),
            ),
            _ => None,
        };
        let resolved_typestate = match (declared, call_state) {
            // Initialized from a call: the call's return state is the truth.
            (Some(decl), Some(Some(actual))) => {
                if decl != actual {
                    let callee = match init {
                        Some(Expr::Call { callee, .. }) => callee.as_str(),
                        _ => unreachable!(),
                    };
                    errors.push(
                        err(format!(
                            "'{name}' is declared as {} but '{callee}' returns {}",
                            fmt_state(&decl),
                            fmt_state(&actual)
                        ))
                        .about(name),
                    );
                }
                Some(actual)
            }
            (Some(decl), Some(None)) => {
                let callee = match init {
                    Some(Expr::Call { callee, .. }) => callee.as_str(),
                    _ => unreachable!(),
                };
                errors.push(
                        err(format!(
                    "'{name}' is declared as {} but '{callee}' does not return a typestate value; a state can only come from a function that returns it",
                    fmt_state(&decl)
                ))
                        .about(name),
                    );
                None
            }
            (None, Some(actual)) => actual,
            // Constructed directly (struct literal or no initializer): only
            // the machine's initial state can be asserted.
            (Some(decl), None) => {
                let initial = self.initial_states.get(decl.0.as_str()).copied();
                if initial == Some(decl.1.as_str()) {
                    Some(decl)
                } else {
                    errors.push(
                        err(format!(
                        "'{name}' cannot be created directly in state {}: a new value starts in {}<{}>, and later states can only come from a function that returns them",
                        fmt_state(&decl),
                        decl.0,
                        initial.unwrap_or("?")
                    ))
                        .about(name),
                    );
                    None
                }
            }
            (None, None) => None,
        };

        let mut var = VarState::local(resolved_domain, resolved_typestate, self.cur_span);
        var.ty = ty.cloned();
        var.block = self.block_depth;
        if let Some(d) = resolved_domain {
            var.domain_since = Some((
                self.cur_span,
                format!("'{name}' is declared in @{} here", d.name()),
            ));
        }
        self.locals.insert(name.to_string(), var);
    }

    fn check_borrow_decl(
        &mut self,
        name: &str,
        target: &str,
        mode: BorrowMode,
        errors: &mut Vec<AnalysisError>,
    ) {
        let Some(target_var) = self.locals.get(target) else {
            errors.push(err(format!("unknown entity '{target}' for borrow")));
            return;
        };
        let rejection = if let Some((parts, _)) = &target_var.split_into {
            Some(format!(
                "'{target}' is split into {}; join them back first (cannot borrow it)",
                parts.join(", ")
            ))
        } else if target_var.destroyed {
            Some(format!("entity '{target}' was destroyed; cannot borrow"))
        } else if let Some(how) = &target_var.consumed {
            Some(format!(
                "value '{target}' was already consumed ({how}); cannot borrow"
            ))
        } else if let Some((owner, _)) = &target_var.borrow_of {
            Some(format!(
                "'{target}' is itself a borrow of '{owner}'; borrow '{owner}' directly instead"
            ))
        } else if target_var.read_only && mode == BorrowMode::Write {
            Some(format!(
                "'{target}' is a `read` parameter of '{}'; it can only be borrowed `read`",
                self.fn_name
            ))
        } else {
            target_var.domain.filter(|d| is_device_owned(*d)).map(|d| {
                format!(
                    "entity '{target}' is owned by @{}; sync({target}) before borrowing it",
                    d.name()
                )
            })
        };
        if let Some(msg) = rejection {
            let note = if target_var.destroyed || target_var.consumed.is_some() {
                Self::lifetime_note(target_var, target)
            } else if target_var.borrow_of.is_some() {
                Some((
                    target_var.origin,
                    format!("'{target}' is a borrow taken here"),
                ))
            } else {
                target_var.domain_since.clone()
            };
            errors.push(err(msg).about(target).note_opt(note.as_ref()));
            // Register an inert placeholder so later references to `name`
            // get targeted errors instead of "unknown entity" noise.
            let mut placeholder = VarState::local(None, None, self.cur_span);
            placeholder.rejected_borrow = true;
            self.locals.insert(name.to_string(), placeholder);
            return;
        }

        let conflict = self.borrow_note(target);
        let bs = self.borrows.entry(target.to_string()).or_default();
        let ok = match mode {
            BorrowMode::Write => {
                if bs.writer.is_some() {
                    errors.push(
                        err(format!(
                            "entity '{target}' already has an exclusive write borrow"
                        ))
                        .about(target)
                        .note_opt(conflict.as_ref()),
                    );
                    false
                } else if !bs.readers.is_empty() {
                    errors.push(
                        err(format!(
                            "entity '{target}' already has an active read borrow; cannot take exclusive write borrow"
                        ))
                        .about(target)
                        .note_opt(conflict.as_ref()),
                    );
                    false
                } else {
                    bs.writer = Some(name.to_string());
                    true
                }
            }
            BorrowMode::Read => {
                if bs.writer.is_some() {
                    errors.push(
                        err(format!(
                            "entity '{target}' already has an exclusive write borrow"
                        ))
                        .about(target)
                        .note_opt(conflict.as_ref()),
                    );
                    false
                } else {
                    bs.readers.insert(name.to_string());
                    true
                }
            }
        };

        let (domain, typestate) = (target_var.domain, target_var.typestate.clone());
        let mut var = VarState::local(domain, typestate, self.cur_span);
        var.ty = target_var.ty.clone();
        var.block = self.block_depth;
        if ok {
            var.borrow_of = Some((target.to_string(), mode));
        } else {
            var.rejected_borrow = true;
        }
        // On a rejected borrow, `name` is still a syntactically valid local
        // going forward -- registered as an inert placeholder (no active
        // borrow) so later statements that reference it (e.g. a subsequent
        // `release name;`) get a targeted, sensible error instead of a
        // confusing cascade of "unknown entity" complaints.
        self.locals.insert(name.to_string(), var);
    }

    /// M6: `volatile_read(REG @MMIO)` / `volatile_write(REG @MMIO, value)`
    /// must have exactly `expected_arity` arguments, the first of which
    /// must be an `@MMIO` domain reference -- not an arbitrary expression.
    /// This is what makes "every volatile access targets a real,
    /// consistently-typed simulated register" a checked property rather
    /// than a hopeful convention (codegen, in M6, relies on this having
    /// already been verified).
    fn check_volatile_register_arg(
        &mut self,
        callee: &str,
        args: &[Expr],
        expected_arity: usize,
        errors: &mut Vec<AnalysisError>,
    ) {
        if args.len() != expected_arity {
            errors.push(err(format!(
                "'{callee}' expects {expected_arity} argument{}, found {}",
                if expected_arity == 1 { "" } else { "s" },
                args.len()
            )));
            return;
        }
        if let Some(target) = self.device_ref(&args[0]) {
            self.check_device_access(callee, target, args.get(1), errors);
            return;
        }
        match args.first() {
            Some(Expr::DomainRef { domain, .. }) if *domain == Domain::Mmio => {}
            Some(Expr::DomainRef { name, domain }) => {
                errors.push(err(format!(
                    "'{callee}' requires an @MMIO register reference, but '{name}' is tagged @{}",
                    domain.name()
                )));
            }
            Some(_) => {
                errors.push(err(format!(
                    "'{callee}' requires a register as its first argument: a device register (`Uart0.STATE`, or a field, `Uart0.STATE.TXFULL`) or a simulated one (`REG_NAME @MMIO`)"
                )));
            }
            None => unreachable!("arity already checked above"),
        }
    }

    /// `Dev.REG` or `Dev.REG.FIELD`, where `Dev` is a declared device (and
    /// not shadowed -- device names cannot be reused for locals): the
    /// device and the path after it. `None` if `e` doesn't start with a
    /// device name.
    fn device_ref<'e>(&self, e: &'e Expr) -> Option<DeviceRef<'e>> {
        let p = phase_ast::reg_path(e)?;
        self.devices.get(p.device)?;
        Some(DeviceRef {
            device: p.device,
            reg: p.reg,
            index: p.index,
            field: p.field,
        })
    }

    /// Resolve a device reference to its register (and field), reporting
    /// an unknown name.
    fn resolve_device_ref(
        &self,
        r: &DeviceRef,
        errors: &mut Vec<AnalysisError>,
    ) -> Option<(&'a RegDecl, Option<&'a RegField>)> {
        let dev = self.devices.get(r.device)?;
        let Some(reg) = dev.reg(r.reg) else {
            let known: Vec<&str> = dev.regs.iter().map(|r| r.name.as_str()).collect();
            errors.push(
                err(format!(
                    "device '{}' has no register '{}' (it has {})",
                    r.device,
                    r.reg,
                    known.join(", ")
                ))
                .note(dev.span, format!("'{}' is declared here", r.device)),
            );
            return None;
        };
        // v2.4: an array register needs a constant index in range; a
        // single register takes none.
        match (reg.array, r.index) {
            (None, None) => {}
            (None, Some(_)) => {
                errors.push(
                    err(format!(
                        "'{}.{}' is a single register, not an array; drop the `[..]`",
                        r.device, r.reg
                    ))
                    .note(
                        reg.span,
                        format!("'{}.{}' is declared here", r.device, r.reg),
                    ),
                );
                return None;
            }
            (Some((count, _)), None) => {
                errors.push(
                    err(format!(
                        "'{}.{}' is an array of {count} registers; pick one: {}.{}[0] to {}.{}[{}]",
                        r.device,
                        r.reg,
                        r.device,
                        r.reg,
                        r.device,
                        r.reg,
                        count.saturating_sub(1)
                    ))
                    .note(
                        reg.span,
                        format!("'{}.{}' is declared here", r.device, r.reg),
                    ),
                );
                return None;
            }
            (Some((count, _)), Some(index)) => match self.const_value(index) {
                Ok(Some(i)) if i < u64::from(count) => {}
                Ok(Some(i)) => {
                    errors.push(
                        err(format!(
                            "{}.{}[{i}] is out of range: '{}.{}' has {count} registers (0 to {})",
                            r.device,
                            r.reg,
                            r.device,
                            r.reg,
                            count.saturating_sub(1)
                        ))
                        .note(
                            reg.span,
                            format!("'{}.{}' is declared here", r.device, r.reg),
                        ),
                    );
                    return None;
                }
                Ok(None) => {
                    errors.push(err(format!(
                        "the index in {} must be a constant: an integer or a `const`",
                        r.reg_text()
                    )));
                    return None;
                }
                Err(msg) => {
                    errors.push(err(msg));
                    return None;
                }
            },
        }
        let Some(fname) = r.field else {
            return Some((reg, None));
        };
        match reg.field(fname) {
            Some(f) => Some((reg, Some(f))),
            None => {
                let known: Vec<&str> = reg.fields.iter().map(|f| f.name.as_str()).collect();
                errors.push(
                    err(format!(
                        "register '{}.{}' has no field '{fname}'{}",
                        r.device,
                        r.reg,
                        if known.is_empty() {
                            " (it declares no fields)".to_string()
                        } else {
                            format!(" (it has {})", known.join(", "))
                        }
                    ))
                    .note(
                        reg.span,
                        format!("'{}.{}' is declared here", r.device, r.reg),
                    ),
                );
                None
            }
        }
    }

    /// v2.0: `volatile_read(Dev.REG[.FIELD])` needs a readable register;
    /// `volatile_write(Dev.REG[.FIELD], v)` a writable one -- and, for a
    /// field, a readable one too, since the other bits are kept by reading
    /// the register first. A literal value must fit the register or field.
    fn check_device_access(
        &mut self,
        callee: &str,
        r: DeviceRef,
        value: Option<&Expr>,
        errors: &mut Vec<AnalysisError>,
    ) {
        let Some((reg, field)) = self.resolve_device_ref(&r, errors) else {
            return;
        };
        let reg_name = r.reg_text();
        let name = match field {
            Some(f) => format!("{reg_name}.{}", f.name),
            None => reg_name.clone(),
        };
        let declared = format!("'{}.{}' is declared here", r.device, reg.name);
        // v2.4: fields whose bits change when the register is read or
        // written back. `other` excludes the field being accessed.
        let other = |kind: FieldKind| {
            reg.fields
                .iter()
                .find(|f| f.kind == kind && Some(f.name.as_str()) != field.map(|x| x.name.as_str()))
        };
        if callee == "volatile_read" {
            if !reg.readable {
                errors.push(
                    err(format!("'{reg_name}' is write-only; it cannot be read"))
                        .note(reg.span, declared),
                );
            } else if let (Some(_), Some(rc)) = (field, other(FieldKind::Rc)) {
                errors.push(
                    err(format!(
                        "reading field '{name}' reads all of '{reg_name}', which clears its `rc` field '{}'; read the register once into a local and take the fields from it",
                        rc.name
                    ))
                    .note(rc.span, format!("'{}' is cleared by reading, declared here", rc.name)),
                );
            }
            return;
        }
        if !reg.writable {
            errors.push(
                err(format!("'{reg_name}' is read-only; it cannot be written"))
                    .note(reg.span, declared),
            );
            return;
        }
        if field.is_some() && !reg.readable {
            errors.push(
                err(format!(
                    "cannot write field '{name}': '{reg_name}' is write-only, and writing one field reads the register to keep its other bits; write the whole register instead"
                ))
                .note(reg.span, declared),
            );
            return;
        }
        // v2.4: a field write reads the register and writes the rest back.
        if field.is_some() {
            let w1c = other(FieldKind::W1c).map(|f| {
                (
                    f,
                    "writes back a 1 to every set bit of `w1c` field",
                    "clearing a pending flag nobody handled",
                )
            });
            let rc_w0 = other(FieldKind::RcW0).map(|f| {
                (
                    f,
                    "writes back the value it read to `rc_w0` field",
                    "so a flag the hardware sets in between is cleared by the 0 written back",
                )
            });
            let rc = reg
                .fields
                .iter()
                .find(|f| f.kind == FieldKind::Rc)
                .map(|f| {
                    (
                        f,
                        "reads it first, which clears `rc` field",
                        "losing what it held",
                    )
                });
            let hazard = w1c.or(rc_w0).or(rc);
            if let Some((f, what, why)) = hazard {
                errors.push(
                    err(format!(
                        "writing field '{name}' {what} '{}' of '{reg_name}', {why}; write the whole register",
                        f.name
                    ))
                    .note(f.span, format!("'{}' is `{}`, declared here", f.name, f.kind.keyword())),
                );
                return;
            }
        }
        let bits = field.map(field_bits).or(reg.bits()).unwrap_or(32);
        let max = if bits >= 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let Some(value) = value else { return };
        // A literal, or (v2.4) any constant expression: its value is known
        // now, so it must fit now.
        let known = match value {
            Expr::IntLit(v) if *v < 0 => Some(Err(*v)),
            Expr::IntLit(v) => Some(Ok(*v as u64)),
            other => {
                self.check_expr(other, errors);
                self.const_value(other).ok().flatten().map(Ok)
            }
        };
        let shown = |v: u64| match value {
            Expr::IntLit(_) => v.to_string(),
            _ => format!("{v} ({v:#x})"),
        };
        let bad = match known {
            Some(Err(v)) => Some(v.to_string()),
            Some(Ok(v)) if v > max => Some(shown(v)),
            _ => None,
        };
        if let Some(v) = bad {
            errors.push(
                err(format!(
                    "{v} does not fit in '{name}' ({bits} bit{}, at most {max})",
                    if bits == 1 { "" } else { "s" }
                ))
                .note(
                    field.map_or(reg.span, |f| f.span),
                    format!("'{name}' is declared here"),
                ),
            );
        }
    }

    /// A bare name used as a *value* (a condition, an assignment right-hand
    /// side, a `volatile_write` value): it must exist, and it must be a
    /// plain scalar -- an entity has no scalar value to read.
    /// v2.4: the value of `e` if it is constant, reporting what makes a
    /// constant expression invalid (a shift by 64, a division by zero...).
    fn const_value(&self, e: &Expr) -> Result<Option<u64>, String> {
        let lookup = |n: &str| {
            if self.locals.contains_key(n) {
                None
            } else {
                self.consts.get(n).map(|v| Ok(*v))
            }
        };
        eval_const(e, &lookup)
    }

    fn check_const_expr(&self, e: &Expr, errors: &mut Vec<AnalysisError>) {
        if let Err(msg) = self.const_value(e) {
            errors.push(err(msg));
        }
    }

    /// Bit operations work on integers: not on a float, a bool, or a
    /// comparison.
    fn check_bit_operand(&self, e: &Expr, op: &str, errors: &mut Vec<AnalysisError>) {
        let what = match e {
            Expr::FloatLit(_) => Some("a float".to_string()),
            Expr::BoolLit(_) => Some("a bool".to_string()),
            Expr::Binary { op: inner, .. }
                if matches!(
                    inner,
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq
                ) =>
            {
                Some(format!("a comparison (`{}`)", inner.symbol()))
            }
            Expr::Ident(n) => match self.locals.get(n).and_then(|v| v.ty.as_ref()) {
                Some(TypeExpr::Named(t)) if matches!(t.as_str(), "f32" | "f64" | "bool") => {
                    Some(format!("'{n}', a {t}"))
                }
                _ => None,
            },
            _ => None,
        };
        if let Some(what) = what {
            errors.push(err(format!(
                "`{op}` works on integers, but its operand is {what}"
            )));
        }
    }

    fn check_value_ident(&self, name: &str, errors: &mut Vec<AnalysisError>) {
        match self.locals.get(name) {
            None if self.consts.contains_key(name) => {}
            None if self.devices.contains_key(name) => errors.push(err(format!(
                "'{name}' is a device, not a value; access its registers with volatile_read/volatile_write"
            ))),
            None => errors.push(err(format!("unknown name '{name}'")).about(name)),
            Some(var) if var.is_tracked() => errors.push(
                err(format!(
                    "'{name}' is an entity, not a scalar value; it cannot be used in an expression"
                ))
                .about(name)
                .note(var.origin, format!("'{name}' is declared here")),
            ),
            Some(var) if var.destroyed => errors.push(
                err(format!("'{name}' was destroyed; cannot use it"))
                    .about(name)
                    .note_opt(Self::lifetime_note(var, name).as_ref()),
            ),
            Some(_) => {}
        }
    }

    /// Shared access check for using an entity's *contents* directly
    /// (call argument, field read): it must be live, owned by the CPU, and
    /// not currently lent out through a borrow.
    /// `read_access`: the use only reads the contents (a `read` parameter,
    /// a field read), which is compatible with outstanding read borrows.
    fn check_entity_access(
        &self,
        name: &str,
        usage: &str,
        read_access: bool,
        errors: &mut Vec<AnalysisError>,
    ) -> bool {
        let Some(var) = self.locals.get(name) else {
            return false;
        };
        let lifetime = Self::lifetime_note(var, name);
        if let Some(e) = Self::split_error(var, name, usage) {
            errors.push(e);
            return false;
        }
        if var.destroyed {
            errors.push(
                err(format!("entity '{name}' was destroyed; cannot {usage}"))
                    .about(name)
                    .note_opt(lifetime.as_ref()),
            );
            return false;
        }
        if let Some(how) = &var.consumed {
            errors.push(
                err(format!(
                    "value '{name}' was already consumed ({how}); cannot {usage}"
                ))
                .about(name)
                .note_opt(lifetime.as_ref()),
            );
            return false;
        }
        if let Some((target, _)) = &var.borrow_of {
            if var.released {
                errors.push(
                    err(format!(
                        "borrow '{name}' of '{target}' was already released; cannot {usage}"
                    ))
                    .about(name)
                    .note_opt(lifetime.as_ref()),
                );
                return false;
            }
        } else if let Some(bs) = self
            .borrows
            .get(name)
            .filter(|b| !b.is_empty() && !(read_access && b.writer.is_none()))
        {
            errors.push(
                err(format!(
                    "entity '{name}' is borrowed by '{}'; it cannot be used directly until that borrow is released",
                    bs.any_holder().unwrap_or_default()
                ))
                .about(name)
                .note_opt(self.borrow_note(name).as_ref()),
            );
            return false;
        }
        true
    }

    fn check_expr(&mut self, expr: &Expr, errors: &mut Vec<AnalysisError>) {
        match expr {
            Expr::Call { callee, args } => self.check_call(callee, args, errors),
            Expr::Ident(name) => self.check_value_ident(name, errors),
            Expr::FieldAccess { .. } | Expr::Index { .. } if self.device_ref(expr).is_some() => {
                let r = self.device_ref(expr).unwrap();
                let path = match r.field {
                    Some(f) => format!("{}.{f}", r.reg_text()),
                    None => r.reg_text(),
                };
                errors.push(err(format!(
                    "'{path}' is a device register; read it with volatile_read({path})"
                )));
            }
            Expr::FieldAccess { base, .. } => match base.as_ref() {
                Expr::Ident(name) => match self.locals.get(name) {
                    None => errors.push(err(format!("unknown name '{name}'")).about(name)),
                    Some(var) => {
                        let domain = var.domain;
                        let since = var.domain_since.clone();
                        if self.check_entity_access(name, "read its fields", true, errors) {
                            if let Some(d) = domain.filter(|d| is_device_owned(*d)) {
                                errors.push(
                                    err(format!(
                                        "entity '{name}' is owned by @{}; sync({name}) before reading its fields",
                                        d.name()
                                    ))
                                    .about(name)
                                    .note_opt(since.as_ref()),
                                );
                            }
                        }
                    }
                },
                other => self.check_expr(other, errors),
            },
            // M11: a condition like `volatile_read(READY @MMIO) == 0`
            // parses as `Binary { lhs: Call(..), rhs: IntLit(0) }`, not a
            // bare `Call` -- without recursing here, the M6 domain/arity
            // checks in check_call would silently never run on a
            // volatile_read wrapped in a comparison.
            Expr::Binary { op, lhs, rhs } => {
                self.check_expr(lhs, errors);
                self.check_expr(rhs, errors);
                if op.is_bitwise() {
                    self.check_bit_operand(lhs, op.symbol(), errors);
                    self.check_bit_operand(rhs, op.symbol(), errors);
                }
                // A shift by a known amount of 64 or more always gives 0
                // (or, of a constant, is an error below): surely a mistake.
                if matches!(op, BinOp::Shl | BinOp::Shr)
                    && self.const_value(lhs).ok().flatten().is_none()
                {
                    if let Ok(Some(n)) = self.const_value(rhs) {
                        if n >= 64 {
                            errors.push(err(format!(
                                "`{}` by {n}: a shift must be less than 64",
                                op.symbol()
                            )));
                        }
                    }
                }
                self.check_const_expr(expr, errors);
            }
            Expr::BitNot(x) => {
                self.check_expr(x, errors);
                self.check_bit_operand(x, "~", errors);
                self.check_const_expr(expr, errors);
            }
            Expr::Index { .. } => errors.push(err(
                "only register arrays can be indexed (`Dev.REG[n]`); PHASE has no other arrays yet",
            )),
            Expr::StructLit { fields, .. } => {
                for (_, e) in fields {
                    self.check_expr(e, errors);
                }
            }
            Expr::Borrow { target, .. } => errors.push(err(format!(
                "`borrow {target}` is only allowed as the initializer of a `let`"
            ))),
            Expr::DomainRef { name, .. } => errors.push(err(format!(
                "register reference '{name}' is only allowed as the first argument of volatile_read/volatile_write"
            ))),
            Expr::IntLit(_) | Expr::FloatLit(_) | Expr::StringLit(_) | Expr::BoolLit(_) => {}
        }
    }

    fn check_call(&mut self, callee: &str, args: &[Expr], errors: &mut Vec<AnalysisError>) {
        // `volatile_read`/`volatile_write` are builtins, not user/extern
        // fns. M6: their argument shape is validated here -- the parser
        // accepts any expression in an argument position, it doesn't know
        // these two callees are special, so nothing before this point has
        // actually confirmed the register argument is an `@MMIO` domain
        // reference.
        if callee == "volatile_read" {
            self.check_volatile_register_arg(callee, args, 1, errors);
            return;
        }
        if callee == "volatile_write" {
            self.check_volatile_register_arg(callee, args, 2, errors);
            if let Some(value) = args.get(1) {
                self.check_expr(value, errors);
            }
            return;
        }

        let Some(sig) = self.fn_sigs.get(callee) else {
            errors.push(err(format!("call to unknown function '{callee}'")));
            return;
        };
        let sig = sig.clone();

        if args.len() != sig.param_domains.len() {
            errors.push(err(format!(
                "'{callee}' expects {} argument{}, found {}",
                sig.param_domains.len(),
                if sig.param_domains.len() == 1 {
                    ""
                } else {
                    "s"
                },
                args.len()
            )));
        }

        // Aliasing: every argument that names an entity is resolved to the
        // storage it refers to (a borrow handle resolves to its target).
        // The same storage may appear more than once only if every
        // occurrence is read-only -- a `read` parameter or a read borrow --
        // otherwise the callee would see two names for memory at least one
        // of them may write.
        let mut roots: Vec<(String, bool)> = Vec::new(); // (root, read_only)
        let mut to_consume: Vec<String> = Vec::new();
        // (name, new domain) for arguments to `@FROM -> @TO` parameters.
        let mut to_transition: Vec<(String, Domain)> = Vec::new();

        for (i, arg) in args.iter().enumerate() {
            match arg {
                Expr::Ident(name) => {
                    let Some(var) = self.locals.get(name).cloned() else {
                        errors.push(
                            err(format!("unknown entity '{name}' passed to '{callee}'"))
                                .about(name),
                        );
                        continue;
                    };
                    let access = sig.param_access.get(i).copied().flatten();
                    let pname = sig.param_names.get(i).cloned().unwrap_or_default();
                    if !self.check_entity_access(
                        name,
                        &format!("use as argument to '{callee}'"),
                        access == Some(BorrowMode::Read),
                        errors,
                    ) {
                        continue;
                    }

                    // Buffer arguments must match the parameter's element type
                    // and length; a `buffer<T, _>` parameter accepts any
                    // length, but a run-time length only fits another `_`.
                    if let Some(param_ty) = sig.param_types.get(i) {
                        if let Some(msg) = arg_type_mismatch(param_ty, var.ty.as_ref()) {
                            errors.push(
                                err(format!(
                                    "'{callee}' expects {} for '{pname}', but '{name}' is {msg}",
                                    fmt_type(param_ty)
                                ))
                                .about(name)
                                .note(var.origin, format!("'{name}' is declared here")),
                            );
                            continue;
                        }
                    }

                    // Read-only data (a read borrow or a `read` parameter)
                    // may only go where the callee promises not to write.
                    if var.is_tracked() && var.is_read_only() {
                        let what = if var.read_only {
                            format!("'{name}' is declared `read` here")
                        } else {
                            format!("'{name}' is a read borrow taken here")
                        };
                        match access {
                            Some(BorrowMode::Write) => errors.push(
                                err(format!(
                                    "'{callee}' may write its parameter '{pname}', but '{name}' is read-only"
                                ))
                                .about(name)
                                .note(var.origin, what),
                            ),
                            // A warning in 1.x, so 1.1 code kept compiling;
                            // an error since 2.0.
                            None => errors.push(
                                err(format!(
                                    "'{callee}' doesn't declare whether it writes its parameter '{pname}', but '{name}' is read-only; mark the parameter `read` if '{callee}' only reads it (or `write` if it writes, and pass it data it may write)"
                                ))
                                .about(name)
                                .note(var.origin, what),
                            ),
                            Some(BorrowMode::Read) => {}
                        }
                    }

                    let mut domain_ok = true;
                    if let Some(Some(param_domain)) = sig.param_domains.get(i) {
                        if let Some(var_domain) = var.domain {
                            if var_domain != *param_domain {
                                domain_ok = false;
                                errors.push(
                                    err(format!(
                                        "entity '{name}' is in domain @{} but '{callee}' expects @{} for this argument",
                                        var_domain.name(),
                                        param_domain.name()
                                    ))
                                    .about(name)
                                    .note_opt(var.domain_since.as_ref()),
                                );
                            }
                        }
                    }

                    // `@FROM -> @TO` parameter: the call changes the
                    // argument's domain. Only an owner can be handed over
                    // that way -- a borrow can't move the storage it views.
                    if let Some(Some(after)) = sig.param_domains_after.get(i) {
                        if var.read_only {
                            errors.push(
                                err(format!(
                                    "'{callee}' moves its argument to @{}, but '{name}' is a `read` parameter",
                                    after.name()
                                ))
                                .about(name)
                                .note(var.origin, format!("'{name}' is declared `read` here")),
                            );
                        } else if var.borrow_of.is_some() {
                            errors.push(
                                err(format!(
                                    "'{callee}' moves its argument to @{}, but '{name}' is only a borrow",
                                    after.name()
                                ))
                                .about(name)
                                .note(var.origin, format!("'{name}' is borrowed here")),
                            );
                        } else if domain_ok && var.domain.is_some() {
                            to_transition.push((name.clone(), *after));
                        }
                    }

                    if let Some(Some(required)) = sig.param_states.get(i) {
                        let (req_machine, req_state) = required;
                        match &var.typestate {
                            Some((have_machine, have_state)) => {
                                let origin_note = (
                                    var.origin,
                                    format!("'{name}' is {have_machine}<{have_state}> from here"),
                                );
                                if have_machine != req_machine {
                                    errors.push(
                                        err(format!(
                                            "entity '{name}' has typestate '{have_machine}' but '{callee}' expects typestate '{req_machine}'"
                                        ))
                                        .about(name)
                                        .note_opt(Some(&origin_note)),
                                    );
                                } else if have_state != req_state {
                                    errors.push(
                                        err(format!(
                                            "expected state {req_state}, found {have_state}"
                                        ))
                                        .about(name)
                                        .note_opt(Some(&origin_note)),
                                    );
                                }
                            }
                            None => errors.push(
                                err(format!(
                                    "'{callee}' requires {} but '{name}' has no proven typestate",
                                    fmt_state(required)
                                ))
                                .about(name),
                            ),
                        }
                        // Linear: handing a value to a state-requiring
                        // parameter consumes it -- unless the parameter is
                        // `read`, which only inspects it. A borrow handle
                        // cannot be consumed (it doesn't own the value).
                        if access == Some(BorrowMode::Read) {
                            // inspected, not consumed
                        } else if var.borrow_of.is_some() || var.read_only {
                            errors.push(
                                err(format!(
                                    "'{callee}' takes ownership of a {} value, but '{name}' is only a borrow",
                                    fmt_state(required)
                                ))
                                .about(name),
                            );
                        } else {
                            to_consume.push(name.clone());
                        }
                    }

                    if var.is_tracked() {
                        let root = match &var.borrow_of {
                            Some((target, _)) => target.clone(),
                            None => name.clone(),
                        };
                        let read_only = access == Some(BorrowMode::Read) || var.is_read_only();
                        roots.push((root, read_only));
                    }
                }
                other => self.check_expr(other, errors),
            }
        }

        let mut reported = HashSet::new();
        for (idx, (root, _)) in roots.iter().enumerate() {
            let same: Vec<&(String, bool)> = roots.iter().filter(|(r, _)| r == root).collect();
            if same.len() > 1
                && !same.iter().all(|(_, ro)| *ro)
                && roots[..idx].iter().all(|(r, _)| r != root)
                && reported.insert(root.clone())
            {
                errors.push(
                    err(format!(
                        "entity '{root}' is passed to '{callee}' more than once; aliased arguments are only allowed when every use is read-only (`read` parameters or read borrows)"
                    ))
                    .about(root),
                );
            }
        }

        let span = self.cur_span;
        for (name, after) in to_transition {
            if let Some(v) = self.locals.get_mut(&name) {
                v.domain = Some(after);
                let how = if after == Domain::Ram {
                    format!("'{name}' is synced back to @RAM by '{callee}' here")
                } else {
                    format!("'{name}' is moved to @{} by '{callee}' here", after.name())
                };
                v.domain_since = Some((span, how));
            }
        }
        for name in to_consume {
            if let Some(v) = self.locals.get_mut(&name) {
                v.consumed = Some(format!("passed to '{callee}'"));
                v.consumed_at = Some(span);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_src(src: &str) -> Result<(), Vec<AnalysisError>> {
        let program = phase_parser::parse(src).expect("test fixture must parse");
        analyze(&program)
    }

    fn assert_single_error_containing(src: &str, needle: &str) {
        match check_src(src) {
            Ok(()) => panic!("expected an analysis error containing {needle:?}, got Ok(())"),
            Err(errs) => {
                assert!(
                    errs.iter().any(|e| e.message.contains(needle)),
                    "expected an error containing {needle:?}, got: {errs:?}"
                );
            }
        }
    }

    const BITS_DEV: &str = r#"
        device Fz @ 0x40000000 {
            CTRL: read write u32 @ 0x00 = 0 { EN: bit 0, MODE: bits 1..=3 }
            LOAD: write u8 @ 0x04;
        }
        extern fn report(n: u32);
    "#;

    fn bits(body: &str) -> String {
        format!("{BITS_DEV}\n{body}")
    }

    #[test]
    fn constants_and_bit_operations_are_accepted() {
        check_src(&bits(
            "const EN: u32 = 1 << 0;
             const ON: u32 = EN | 5 << 1;
             const ALL: u8 = 0xFF;
             fn main() {
                 volatile_write(Fz.CTRL, ON);
                 volatile_write(Fz.LOAD, ALL & ~0x0F);
                 volatile_write(Fz.CTRL.MODE, ON >> 1);
                 let c: u32 = volatile_read(Fz.CTRL);
                 let m: u32 = c >> 1 & 7;
                 if c & EN == 1 { report(m ^ 1); }
             }",
        ))
        .unwrap();
        // Constants may refer to ones declared later.
        check_src("const A: u32 = B + 1; const B: u32 = 2; fn main() {}").unwrap();
    }

    #[test]
    fn constant_values_must_fit_and_be_computable() {
        assert_single_error_containing(
            "const BIG: u8 = 1 << 8; fn main() {}",
            "constant 'BIG' is 256 (0x100), which does not fit in u8 (at most 0xff)",
        );
        assert_single_error_containing(
            "const S: u64 = 1 << 64; fn main() {}",
            "a shift must be less than 64",
        );
        assert_single_error_containing(
            "const D: u32 = 4 / (2 - 2); fn main() {}",
            "divides by zero",
        );
        assert_single_error_containing("const N: u32 = 1 - 2; fn main() {}", "1 - 2 is negative");
        assert_single_error_containing(
            "const A: u32 = B; const B: u32 = A; fn main() {}",
            "is defined in terms of itself",
        );
        assert_single_error_containing(
            "const F: f32 = 1; fn main() {}",
            "constants are u8, u16, u32 or u64",
        );
        assert_single_error_containing(
            "fn main() { let x: u32 = 1; } const C: u32 = x;",
            "constant 'C' must be computed from literals and other constants",
        );
        assert_single_error_containing(
            "const C: u32 = 1; const C: u32 = 2; fn main() {}",
            "constant 'C' is declared more than once",
        );
        assert_single_error_containing(
            "const main: u32 = 1; fn main() {}",
            "'main' is declared as a constant and as something else",
        );
        assert_single_error_containing(
            "const N: u32 = 1; fn main() { let N: u32 = 2; }",
            "'N' is a constant; choose another name for this local",
        );
    }

    #[test]
    fn a_computed_constant_must_fit_its_register() {
        assert_single_error_containing(
            &bits("const HI: u32 = 0xF0; fn main() { volatile_write(Fz.LOAD, HI << 4); }"),
            "3840 (0xf00) does not fit in 'Fz.LOAD' (8 bits, at most 255)",
        );
        assert_single_error_containing(
            &bits("fn main() { volatile_write(Fz.CTRL.MODE, 1 << 3); }"),
            "8 (0x8) does not fit in 'Fz.CTRL.MODE' (3 bits, at most 7)",
        );
        // ~ flips all 64 bits: mask it.
        assert_single_error_containing(
            &bits("fn main() { volatile_write(Fz.CTRL, ~1); }"),
            "does not fit in 'Fz.CTRL' (32 bits",
        );
    }

    #[test]
    fn bit_operations_need_integers() {
        assert_single_error_containing(
            "fn main() { let f: f32 = 1.5; let x: u32 = 1; x = x | f; }",
            "`|` works on integers, but its operand is 'f', a f32",
        );
        assert_single_error_containing(
            "fn main() { let x: u32 = 1; x = ~(x == 1); }",
            "`~` works on integers, but its operand is a comparison (`==`)",
        );
        assert_single_error_containing(
            "fn main() { let x: u32 = 1; x = x << 64; }",
            "a shift must be less than 64",
        );
    }

    const DMA_DEV: &str = r#"
        device Dma @ 0x40026000 {
            LISR:  read u32 @ 0x00 { TCIF0: bit 5, TCIF1: bit 11 }
            LIFCR: write u32 @ 0x08;
            CR[8]:   read write u32 @ 0x10 step 0x18 { EN: bit 0, TCIE: bit 4, CHSEL: bits 25..=27 }
            NDTR[8]: read write u32 @ 0x14 step 0x18 { N: bits 0..=15 }
            INT:   read write u32 @ 0x100 { PENDING: bit 0 w1c, OVERRUN: bit 1 w1c, ENABLE: bit 4 }
            SR:    read write u16 @ 0x104 { UIF: bit 0 rc_w0, CC1IF: bit 1 rc_w0 }
            DR:    read u32 @ 0x108 { DATA: bits 0..=8, ERR: bit 9 rc, BREAK: bit 10 rc }
        }
        const STREAM: u32 = 3;
    "#;

    fn dma(body: &str) -> String {
        format!("{DMA_DEV}\n{body}")
    }

    #[test]
    fn register_arrays_are_indexed_by_constants() {
        check_src(&dma("fn main() {
                 volatile_write(Dma.CR[0].EN, 1);
                 volatile_write(Dma.CR[STREAM].CHSEL, 7);
                 volatile_write(Dma.NDTR[STREAM + 4], 16);
                 let n: u32 = volatile_read(Dma.NDTR[7].N);
             }"))
        .unwrap();
        assert_single_error_containing(
            &dma("fn main() { volatile_write(Dma.CR[8].EN, 1); }"),
            "Dma.CR[8] is out of range: 'Dma.CR' has 8 registers (0 to 7)",
        );
        assert_single_error_containing(
            &dma("fn main() { volatile_write(Dma.CR.EN, 1); }"),
            "'Dma.CR' is an array of 8 registers; pick one: Dma.CR[0] to Dma.CR[7]",
        );
        assert_single_error_containing(
            &dma("fn main() { let i: u32 = 1; volatile_write(Dma.CR[i].EN, 1); }"),
            "the index in Dma.CR[i] must be a constant",
        );
        assert_single_error_containing(
            &dma("fn main() { volatile_write(Dma.LIFCR[0], 1); }"),
            "'Dma.LIFCR' is a single register, not an array",
        );
        assert_single_error_containing(
            "device D @ 0x40000000 { A[4]: read write u32 @ 0x0 step 2; } fn main() {}",
            "has a step of 2 bytes, less than its 4-byte registers",
        );
        assert_single_error_containing(
            "device D @ 0x40000000 { A[4]: read write u32 @ 0x0 step 8; B: read u32 @ 0x10; } fn main() {}",
            "register 'D.B' (bytes 0x10..0x14) overlaps 'D.A'",
        );
    }

    #[test]
    fn a_field_write_must_not_clear_other_flags() {
        assert_single_error_containing(
            &dma("fn main() { volatile_write(Dma.INT.ENABLE, 1); }"),
            "writing field 'Dma.INT.ENABLE' writes back a 1 to every set bit of `w1c` field 'PENDING' of 'Dma.INT', clearing a pending flag nobody handled; write the whole register",
        );
        // Clearing one flag by a field write still writes the other back.
        assert_single_error_containing(
            &dma("fn main() { volatile_write(Dma.INT.PENDING, 1); }"),
            "`w1c` field 'OVERRUN'",
        );
        assert_single_error_containing(
            &dma("fn main() { volatile_write(Dma.SR.UIF, 0); }"),
            "writes back the value it read to `rc_w0` field 'CC1IF' of 'Dma.SR', so a flag the hardware sets in between is cleared",
        );
        assert_single_error_containing(
            &dma("fn main() { let e: u32 = volatile_read(Dma.DR.ERR); }"),
            "reading field 'Dma.DR.ERR' reads all of 'Dma.DR', which clears its `rc` field 'BREAK'",
        );
        // The right ways: whole-register writes and reads.
        check_src(&dma("fn main() {
                 volatile_write(Dma.INT, 1 << 0 | 1 << 4);
                 volatile_write(Dma.SR, 0xFFFF & ~1);
                 let dr: u32 = volatile_read(Dma.DR);
                 let err: u32 = dr >> 9 & 1;
                 let pending: u32 = volatile_read(Dma.INT.PENDING);
             }"))
        .unwrap();
    }

    #[test]
    fn handler_races_are_per_array_element() {
        let handler =
            "event e; interrupt fn h() @ irq 3 { volatile_write(Dma.CR[1].TCIE, 0); signal e; }";
        assert_single_error_containing(
            &dma(&format!(
                "{handler} fn main() {{ volatile_write(Dma.CR[1].EN, 1); }}"
            )),
            "also writes 'Dma.CR[1]'",
        );
        check_src(&dma(&format!(
            "{handler} fn main() {{ volatile_write(Dma.CR[2].EN, 1); }}"
        )))
        .unwrap();
    }

    const HAPPY_PATH: &str = r#"
        entity Sample : @RAM { value: f32 }

        extern fn fir_filter(in_: buffer<Sample, 1024> @RAM, out: buffer<Sample, 1024> @RAM);
        extern fn device_capture(dst: buffer<Sample, 1024> @DMA);
        extern fn device_playback(src: buffer<Sample, 1024> @DEVICE);

        fn main() {
            buffer<Sample, 1024> raw @DMA;
            device_capture(raw);
            sync(raw);

            buffer<Sample, 1024> filtered @RAM;
            fir_filter(raw, filtered);

            move filtered -> @DEVICE;
            device_playback(filtered);
            sync(filtered);
        }
    "#;

    #[test]
    fn happy_path_pipeline_is_accepted() {
        assert_eq!(check_src(HAPPY_PATH), Ok(()));
    }

    // ---- Bug gallery #1: use of a DeviceOwned entity without sync -------

    #[test]
    fn bug1_use_dma_buffer_without_sync_is_rejected() {
        let src = r#"
            extern fn fir_filter(in_: buffer<Sample, 1024> @RAM, out: buffer<Sample, 1024> @RAM);
            entity Sample : @RAM { value: f32 }
            fn main() {
                buffer<Sample, 1024> raw @DMA;
                buffer<Sample, 1024> filtered @RAM;
                fir_filter(raw, filtered);
            }
        "#;
        assert_single_error_containing(src, "is in domain @DMA but 'fir_filter' expects @RAM");
    }

    // ---- Bug gallery #2: move/destroy while a borrow is live ------------

    #[test]
    fn bug2_move_while_borrowed_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                let v = borrow x read;
                move x -> @DMA;
            }
        "#;
        // move RAM -> DMA is otherwise legal, so this isolates the borrow check
        assert_single_error_containing(src, "has a live borrow; cannot move");
    }

    #[test]
    fn bug2_destroy_while_borrowed_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                let v = borrow x write;
                destroy x;
            }
        "#;
        assert_single_error_containing(src, "has a live borrow; cannot destroy");
    }

    #[test]
    fn release_then_move_is_accepted() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                let v = borrow x read;
                release v;
                move x -> @DMA;
                sync(x);
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    // ---- Bug gallery #3: conflicting borrows -----------------------------

    #[test]
    fn bug3_two_write_borrows_conflict() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                let a = borrow x write;
                let b = borrow x write;
            }
        "#;
        assert_single_error_containing(src, "already has an exclusive write borrow");
    }

    #[test]
    fn bug3_write_borrow_while_read_borrow_live_conflicts() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                let a = borrow x read;
                let b = borrow x write;
            }
        "#;
        assert_single_error_containing(
            src,
            "already has an active read borrow; cannot take exclusive write borrow",
        );
    }

    #[test]
    fn multiple_read_borrows_are_fine() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                let a = borrow x read;
                let b = borrow x read;
                release a;
                release b;
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    // ---- Bug gallery #4: use after destroy -------------------------------

    #[test]
    fn bug4_use_after_destroy_is_rejected() {
        let src = r#"
            extern fn fir_filter(in_: buffer<Sample, 1024> @RAM, out: buffer<Sample, 1024> @RAM);
            entity Sample : @RAM { value: f32 }
            fn main() {
                buffer<Sample, 1024> raw @RAM;
                buffer<Sample, 1024> filtered @RAM;
                destroy raw;
                fir_filter(raw, filtered);
            }
        "#;
        assert_single_error_containing(src, "was destroyed; cannot use as argument");
    }

    #[test]
    fn bug4_double_destroy_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                destroy x;
                destroy x;
            }
        "#;
        assert_single_error_containing(src, "was already destroyed");
    }

    // ---- transition-table checks -----------------------------------------

    #[test]
    fn illegal_move_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @MMIO;
                move x -> @RAM;
            }
        "#;
        assert_single_error_containing(src, "no legal transition from @MMIO to @RAM via move");
    }

    #[test]
    fn sync_from_ram_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                sync(x);
            }
        "#;
        assert_single_error_containing(src, "no legal sync transition from @RAM");
    }

    #[test]
    fn stack_to_ram_move_is_legal() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @STACK;
                move x -> @RAM;
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn unknown_function_call_is_rejected() {
        let src = r#"
            fn main() {
                totally_undeclared_fn();
            }
        "#;
        assert_single_error_containing(src, "call to unknown function 'totally_undeclared_fn'");
    }

    #[test]
    fn buffer_without_domain_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x;
            }
        "#;
        assert_single_error_containing(
            src,
            "has no domain: buffer types require an explicit @domain",
        );
    }

    #[test]
    fn one_bad_statement_does_not_cascade_into_unrelated_errors() {
        // The move is illegal (RAM -> RAM isn't a legal move transition
        // even though the domains happen to match), but everything after
        // it is fine and should not also error.
        let src = r#"
            extern fn fir_filter(in_: buffer<Sample, 1024> @RAM, out: buffer<Sample, 1024> @RAM);
            entity Sample : @RAM { value: f32 }
            fn main() {
                buffer<Sample, 1024> raw @RAM;
                move raw -> @RAM;
                buffer<Sample, 1024> filtered @RAM;
                fir_filter(raw, filtered);
            }
        "#;
        let errs = check_src(src).unwrap_err();
        assert_eq!(errs.len(), 1, "expected exactly one error, got {errs:?}");
    }

    // ---- Bug gallery #5: typestate -----------------------------------------

    const PACKET_MACHINE: &str = r#"
        state Packet {
            Received -> Decoded -> Validated
        }
        extern fn receive_packet() -> Packet<Received>;
        extern fn decode(p: Packet<Received>) -> Packet<Decoded>;
        extern fn validate(p: Packet<Decoded>) -> Packet<Validated>;
        extern fn handle(p: Packet<Validated>);
    "#;

    #[test]
    fn happy_path_typestate_chain_is_accepted() {
        let src = format!(
            r#"
            {PACKET_MACHINE}
            fn main() {{
                let p1 = receive_packet();
                let p2 = decode(p1);
                let p3 = validate(p2);
                handle(p3);
            }}
        "#
        );
        assert_eq!(check_src(&src), Ok(()));
    }

    #[test]
    fn bug5_skipped_state_transition_is_rejected() {
        let src = format!(
            r#"
            {PACKET_MACHINE}
            fn main() {{
                let p1 = receive_packet();
                // BUG: skipped decode() -- p1 is still Received, validate() needs Decoded.
                let p3 = validate(p1);
            }}
        "#
        );
        assert_single_error_containing(&src, "expected state Decoded, found Received");
    }

    #[test]
    fn explicit_stateful_annotation_is_checked_against_the_machine() {
        let src = format!(
            r#"
            {PACKET_MACHINE}
            fn main() {{
                let p1: Packet<Received> = receive_packet();
                let p3 = validate(p1);
            }}
        "#
        );
        assert_single_error_containing(&src, "expected state Decoded, found Received");
    }

    #[test]
    fn unknown_typestate_machine_is_rejected() {
        let src = r#"
            fn main() {
                let p: Ghost<Foo> = receive_packet();
            }
        "#;
        assert_single_error_containing(src, "unknown typestate 'Ghost'");
    }

    #[test]
    fn unknown_state_within_known_machine_is_rejected() {
        let src = r#"
            state Packet {
                Received -> Decoded -> Validated
            }
            fn main() {
                let p: Packet<Bogus> = receive_packet();
            }
        "#;
        assert_single_error_containing(src, "unknown state 'Bogus' for typestate 'Packet'");
    }

    #[test]
    fn duplicate_typestate_declaration_is_rejected() {
        let src = r#"
            state Packet { Received -> Decoded }
            state Packet { A -> B }
        "#;
        assert_single_error_containing(src, "typestate 'Packet' is declared more than once");
    }

    #[test]
    fn duplicate_state_within_machine_is_rejected() {
        let src = r#"
            state Packet { Received -> Received -> Decoded }
        "#;
        assert_single_error_containing(
            src,
            "typestate 'Packet' declares state 'Received' more than once",
        );
    }

    #[test]
    fn returning_wrong_state_is_rejected() {
        let src = format!(
            r#"
            {PACKET_MACHINE}
            fn wrong_decode(p: Packet<Received>) -> Packet<Decoded> {{
                return p;
            }}
        "#
        );
        assert_single_error_containing(&src, "expected state Decoded, found Received");
    }

    // ---- M4: control flow + branch merging ("ENTITY_PHI") -----------------

    #[test]
    fn if_else_with_consistent_domain_on_both_paths_is_accepted() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                if flag {
                    move x -> @DMA;
                    sync(x);
                } else {
                    move x -> @DEVICE;
                    sync(x);
                }
            }
        "#;
        // Both paths independently return x to @RAM by the time they join.
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn bug6_if_else_domain_disagreement_is_rejected() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                if flag {
                    move x -> @DMA;
                } else {
                    move x -> @DEVICE;
                }
            }
        "#;
        // then: x is @DMA, else: x is @DEVICE -- they disagree at the join.
        assert_single_error_containing(
            src,
            "entity 'x' disagrees on domain across incoming branches",
        );
    }

    #[test]
    fn if_without_else_must_agree_with_the_implicit_unchanged_path() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                if flag {
                    move x -> @DMA;
                }
            }
        "#;
        // No else means the implicit "else" is "x is still @RAM" -- which
        // disagrees with the then-branch's @DMA.
        assert_single_error_containing(
            src,
            "entity 'x' disagrees on domain across incoming branches",
        );
    }

    #[test]
    fn destroy_on_only_one_branch_is_rejected() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                if flag {
                    destroy x;
                } else {
                }
            }
        "#;
        assert_single_error_containing(
            src,
            "entity 'x' disagrees on destroyed-state across incoming branches",
        );
    }

    #[test]
    fn variable_declared_inside_a_branch_is_out_of_scope_after_it() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                if flag {
                    buffer<u8, 16> y @RAM;
                }
                move y -> @DMA;
            }
        "#;
        assert_single_error_containing(src, "unknown entity 'y'");
    }

    #[test]
    fn borrow_taken_and_released_within_one_branch_is_fine() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                if flag {
                    let v = borrow x read;
                    release v;
                }
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn borrow_taken_inside_a_branch_and_not_released_is_rejected() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                if flag {
                    let v = borrow x read;
                }
            }
        "#;
        assert_single_error_containing(
            src,
            "borrow of 'x' taken inside a branch must be released before the branch ends",
        );
    }

    #[test]
    fn borrow_from_before_the_branch_released_on_only_one_path_is_rejected() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                let v = borrow x read;
                if flag {
                    release v;
                } else {
                }
            }
        "#;
        assert_single_error_containing(
            src,
            "entity 'x' disagrees on borrow state across incoming branches",
        );
    }

    #[test]
    fn nested_if_inside_if_is_checked() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                if a {
                    if b {
                        move x -> @DMA;
                    } else {
                        move x -> @DEVICE;
                    }
                } else {
                }
            }
        "#;
        // Inner branch already disagrees, regardless of the outer branch.
        assert_single_error_containing(
            src,
            "entity 'x' disagrees on domain across incoming branches",
        );
    }

    #[test]
    fn while_body_consistent_with_pre_loop_state_is_accepted() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                while flag {
                    move x -> @DMA;
                    sync(x);
                }
            }
        "#;
        // One pass through the body returns x to @RAM, matching skipping
        // the loop entirely -- consistent either way.
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn while_body_that_changes_domain_permanently_is_rejected() {
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @RAM;
                while flag {
                    move x -> @DMA;
                }
            }
        "#;
        // Body-exit state (@DMA) disagrees with pre-loop state (@RAM) --
        // unsound if the loop runs a variable number of times.
        assert_single_error_containing(
            src,
            "entity 'x' is @DMA at the end of the `while` body but @RAM when the loop is entered",
        );
    }

    #[test]
    fn if_still_checks_statements_inside_each_branch_normally() {
        // The move inside the branch is illegal on its own terms (MMIO has
        // no legal move target to RAM), independent of any merge issue.
        let src = r#"
            extern fn get_flag() -> bool;
            fn main() {
                let flag: bool = get_flag();
                buffer<u8, 16> x @MMIO;
                if flag {
                    move x -> @RAM;
                } else {
                }
            }
        "#;
        assert_single_error_containing(src, "no legal transition from @MMIO to @RAM via move");
    }

    // ---- M6: volatile MMIO register access ---------------------------------

    #[test]
    fn valid_volatile_round_trip_is_accepted() {
        let src = r#"
            fn main() {
                let status: u32 = volatile_read(UART_STATUS @MMIO);
                volatile_write(UART_CONTROL @MMIO, status);
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn volatile_read_with_non_mmio_domain_ref_is_rejected() {
        let src = r#"
            fn main() {
                let status: u32 = volatile_read(SOME_REG @RAM);
            }
        "#;
        assert_single_error_containing(
            src,
            "'volatile_read' requires an @MMIO register reference, but 'SOME_REG' is tagged @RAM",
        );
    }

    #[test]
    fn volatile_read_with_non_domain_ref_argument_is_rejected() {
        let src = r#"
            fn main() {
                let x: u32 = 5;
                let status: u32 = volatile_read(x);
            }
        "#;
        assert_single_error_containing(
            src,
            "'volatile_read' requires a register as its first argument",
        );
    }

    #[test]
    fn volatile_read_with_wrong_arity_is_rejected() {
        let src = r#"
            fn main() {
                let status: u32 = volatile_read(A @MMIO, B @MMIO);
            }
        "#;
        assert_single_error_containing(src, "'volatile_read' expects 1 argument, found 2");
    }

    #[test]
    fn volatile_write_with_wrong_arity_is_rejected() {
        let src = r#"
            fn main() {
                volatile_write(A @MMIO);
            }
        "#;
        assert_single_error_containing(src, "'volatile_write' expects 2 arguments, found 1");
    }

    #[test]
    fn volatile_write_with_non_mmio_first_arg_is_rejected() {
        let src = r#"
            fn main() {
                volatile_write(A @DMA, 1);
            }
        "#;
        assert_single_error_containing(
            src,
            "'volatile_write' requires an @MMIO register reference, but 'A' is tagged @DMA",
        );
    }

    // ---- M9: assignment ---------------------------------------------------

    #[test]
    fn reassigning_a_scalar_local_is_accepted() {
        let src = r#"
            fn main() {
                let n: i32 = 0;
                n = n + 1;
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn counting_while_loop_with_assignment_is_accepted() {
        let src = r#"
            extern fn tick(n: i32);
            fn main() {
                let n: i32 = 0;
                while n < 5 {
                    tick(n);
                    n = n + 1;
                }
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn assigning_to_an_undeclared_name_is_rejected() {
        let src = r#"
            fn main() {
                n = 1;
            }
        "#;
        assert_single_error_containing(src, "cannot assign to unknown local 'n'");
    }

    #[test]
    fn assigning_to_a_domain_tracked_buffer_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                x = 1;
            }
        "#;
        assert_single_error_containing(
            src,
            "cannot assign to 'x': it is domain-tracked (use move/sync instead)",
        );
    }

    #[test]
    fn assigning_to_a_borrow_handle_is_rejected() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                let v = borrow x read;
                v = x;
            }
        "#;
        assert_single_error_containing(src, "cannot assign to 'v': it is a borrow handle");
    }

    // ---- M11: volatile_read in conditions ----------------------------------

    #[test]
    fn volatile_read_inside_while_condition_is_accepted() {
        let src = r#"
            extern fn arm_if_ready();
            fn main() {
                while volatile_read(READY @MMIO) == 0 {
                    arm_if_ready();
                }
            }
        "#;
        assert_eq!(check_src(src), Ok(()));
    }

    #[test]
    fn volatile_read_inside_while_condition_still_checks_mmio_domain() {
        // M11 doesn't bypass the existing M6 validation -- volatile_read's
        // argument shape/domain checks (check_call, via check_expr) still
        // run on a condition exactly like anywhere else.
        let src = r#"
            fn main() {
                while volatile_read(READY @RAM) == 0 {
                }
            }
        "#;
        assert_single_error_containing(
            src,
            "'volatile_read' requires an @MMIO register reference, but 'READY' is tagged @RAM",
        );
    }
}

/// v1.0 soundness regressions: every hole found while auditing v0.1, each
/// paired with the legitimate program the fix must keep accepting.
#[cfg(test)]
mod soundness_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[track_caller]
    fn rejects(src: &str, needle: &str) {
        match check(src) {
            Ok(()) => panic!("expected an error containing {needle:?}, got Ok(())"),
            Err(errs) => assert!(
                errs.iter().any(|e| e.message.contains(needle)),
                "expected an error containing {needle:?}, got: {errs:?}"
            ),
        }
    }

    #[track_caller]
    fn accepts(src: &str) {
        assert_eq!(check(src), Ok(()));
    }

    const PACKET: &str = r#"
        entity Packet : @RAM { sequence: u32 }
        state Packet { Received -> Decoded -> Validated }
        extern fn receive_packet() -> Packet<Received>;
        extern fn decode(p: Packet<Received>) -> Packet<Decoded>;
        extern fn validate(p: Packet<Decoded>) -> Packet<Validated>;
        extern fn handle(p: Packet<Validated>);
        extern fn raw_len() -> u32;
        extern fn get_flag() -> bool;
    "#;

    fn with_packet(body: &str) -> String {
        format!("{PACKET}\n{body}")
    }

    const BUF: &str = r#"
        extern fn fir(in_: buffer<u8, 16> @RAM, out: buffer<u8, 16> @RAM);
        extern fn peek(x: buffer<u8, 16> @RAM);
        extern fn cap(x: buffer<u8, 16> @DMA);
        extern fn play(x: buffer<u8, 16> @DEVICE);
        extern fn get_flag() -> bool;
    "#;

    fn with_buf(body: &str) -> String {
        format!("{BUF}\n{body}")
    }

    // ---- typestate: annotations are claims, not evidence ------------------

    #[test]
    fn annotation_contradicting_the_call_is_rejected() {
        rejects(
            &with_packet("fn main() { let p: Packet<Validated> = receive_packet(); handle(p); }"),
            "'p' is declared as Packet<Validated> but 'receive_packet' returns Packet<Received>",
        );
    }

    #[test]
    fn annotation_matching_the_call_is_accepted() {
        accepts(&with_packet(
            "fn main() { let p: Packet<Received> = receive_packet(); let d = decode(p); let v = validate(d); handle(v); }",
        ));
    }

    #[test]
    fn annotation_on_a_call_without_typestate_is_rejected() {
        rejects(
            &with_packet("fn main() { let p: Packet<Validated> = raw_len(); handle(p); }"),
            "does not return a typestate value",
        );
    }

    #[test]
    fn struct_literal_may_only_start_in_the_initial_state() {
        accepts(&with_packet(
            "fn main() { let p: Packet<Received> = Packet { sequence: 1 }; let d = decode(p); let v = validate(d); handle(v); }",
        ));
        rejects(
            &with_packet(
                "fn main() { let p: Packet<Validated> = Packet { sequence: 1 }; handle(p); }",
            ),
            "cannot be created directly in state Packet<Validated>",
        );
    }

    #[test]
    fn uninitialized_later_state_is_rejected() {
        rejects(
            &with_packet("fn main() { let p: Packet<Validated>; handle(p); }"),
            "cannot be created directly in state Packet<Validated>",
        );
    }

    #[test]
    fn untracked_value_cannot_satisfy_a_typestate_parameter() {
        rejects(
            &with_packet("fn main() { let n: u32 = raw_len(); handle(n); }"),
            "'handle' requires Packet<Validated> but 'n' has no proven typestate",
        );
    }

    // ---- typestate: values are linear ------------------------------------

    #[test]
    fn reusing_a_consumed_typestate_value_is_rejected() {
        rejects(
            &with_packet("fn main() { let p1 = receive_packet(); let p2 = decode(p1); let p3 = decode(p1); }"),
            "value 'p1' was already consumed (passed to 'decode')",
        );
    }

    #[test]
    fn handling_the_same_validated_packet_twice_is_rejected() {
        rejects(
            &with_packet(
                "fn main() { let a = receive_packet(); let b = decode(a); let c = validate(b); handle(c); handle(c); }",
            ),
            "value 'c' was already consumed (passed to 'handle')",
        );
    }

    #[test]
    fn same_value_twice_in_one_call_is_rejected() {
        let src = r#"
            state Packet { Received -> Decoded }
            extern fn receive_packet() -> Packet<Received>;
            extern fn merge(a: Packet<Received>, b: Packet<Received>);
            fn main() { let p = receive_packet(); merge(p, p); }
        "#;
        rejects(src, "passed to 'merge' more than once");
    }

    #[test]
    fn rebinding_an_entity_with_let_is_rejected() {
        rejects(
            &with_packet("fn main() { let p1 = receive_packet(); let q = p1; let d = decode(q); }"),
            "cannot bind 'q' to entity 'p1'",
        );
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @DMA; let y = x; peek(y); }"),
            "cannot bind 'y' to entity 'x'",
        );
    }

    #[test]
    fn consumed_on_one_branch_then_used_is_rejected() {
        rejects(
            &with_packet(
                "fn main() { let f: bool = get_flag(); let p = receive_packet(); if f { let d = decode(p); let v = validate(d); handle(v); } let again = decode(p); }",
            ),
            "on one branch of an `if`",
        );
    }

    #[test]
    fn consumed_on_one_branch_and_not_used_again_is_accepted() {
        accepts(&with_packet(
            "fn main() { let f: bool = get_flag(); let p = receive_packet(); if f { let d = decode(p); let v = validate(d); handle(v); } }",
        ));
    }

    #[test]
    fn consuming_an_outer_value_inside_a_loop_is_rejected() {
        rejects(
            &with_packet(
                "fn main() { let n: u32 = 0; let p = receive_packet(); while n < 3 { let d = decode(p); n = n + 1; } }",
            ),
            "consumed inside a `while` loop",
        );
    }

    #[test]
    fn fresh_value_per_loop_iteration_is_accepted() {
        accepts(&with_packet(
            "fn main() { let n: u32 = 0; while n < 3 { let p = receive_packet(); let d = decode(p); let v = validate(d); handle(v); n = n + 1; } }",
        ));
    }

    #[test]
    fn phase_fn_returning_the_promised_state_is_accepted() {
        accepts(&with_packet(
            r#"
            fn admit() -> Packet<Validated> {
                let p = receive_packet();
                let d = decode(p);
                let v = validate(d);
                return v;
            }
            fn main() { let v = admit(); handle(v); }
            "#,
        ));
    }

    #[test]
    fn phase_fn_returning_an_earlier_state_is_rejected() {
        rejects(
            &with_packet(
                "fn admit() -> Packet<Validated> { let p = receive_packet(); return p; } fn main() { }",
            ),
            "expected state Validated, found Received",
        );
    }

    #[test]
    fn returning_a_consumed_value_is_rejected() {
        rejects(
            &with_packet(
                "fn admit() -> Packet<Decoded> { let p = receive_packet(); let d = decode(p); return p; } fn main() { }",
            ),
            "value 'p' was already consumed",
        );
    }

    // ---- hardware ownership across destroy / scope exit -------------------

    #[test]
    fn destroy_while_dma_owned_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; move x -> @DMA; destroy x; }"),
            "entity 'x' is still owned by @DMA; sync(x) before destroying it",
        );
    }

    #[test]
    fn destroy_while_device_owned_is_rejected() {
        rejects(
            &with_buf(
                "fn main() { buffer<u8, 16> x @RAM; move x -> @DEVICE; play(x); destroy x; }",
            ),
            "still owned by @DEVICE; sync(x) before destroying it",
        );
    }

    #[test]
    fn destroy_after_sync_is_accepted() {
        accepts(&with_buf(
            "fn main() { buffer<u8, 16> x @RAM; move x -> @DMA; cap(x); sync(x); destroy x; }",
        ));
    }

    #[test]
    fn returning_with_a_dma_owned_local_is_rejected() {
        rejects(
            &with_buf("fn helper() { buffer<u8, 16> x @RAM; move x -> @DMA; cap(x); } fn main() { helper(); }"),
            "entity 'x' is still owned by @DMA when 'helper' returns",
        );
    }

    #[test]
    fn early_return_with_a_dma_owned_local_is_rejected() {
        rejects(
            &with_buf(
                "fn main() { let f: bool = get_flag(); buffer<u8, 16> x @RAM; move x -> @DMA; if f { return; } sync(x); }",
            ),
            "entity 'x' is still owned by @DMA when 'main' returns",
        );
    }

    #[test]
    fn early_return_after_sync_does_not_poison_the_join() {
        // The `then` branch returns, so only the `else` path reaches the
        // join -- no false "disagrees across branches" error.
        accepts(&with_buf(
            r#"
            fn main() {
                let f: bool = get_flag();
                buffer<u8, 16> x @RAM;
                move x -> @DMA;
                cap(x);
                if f {
                    sync(x);
                    return;
                } else {
                    sync(x);
                    move x -> @DEVICE;
                    play(x);
                    sync(x);
                }
                peek(x);
            }
            "#,
        ));
    }

    #[test]
    fn statement_after_return_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; return; peek(x); }"),
            "unreachable statement after `return`",
        );
    }

    #[test]
    fn unreleased_borrow_at_function_exit_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; let v = borrow x read; peek(v); }"),
            "borrow 'v' of 'x' is still live when 'main' returns",
        );
    }

    #[test]
    fn parameter_must_be_handed_back_in_its_entry_domain() {
        rejects(
            &with_buf("fn stage(b: buffer<u8, 16> @RAM) { move b -> @DMA; cap(b); } fn main() { }"),
            "parameter 'b' must be back in @RAM when 'stage' returns, but it is in @DMA",
        );
        accepts(&with_buf(
            "fn stage(b: buffer<u8, 16> @RAM) { move b -> @DMA; cap(b); sync(b); } fn main() { }",
        ));
    }

    #[test]
    fn dma_parameter_left_in_dma_is_the_callers_to_sync() {
        accepts(&with_buf(
            "fn kick(b: buffer<u8, 16> @DMA) { cap(b); } fn main() { }",
        ));
    }

    #[test]
    fn destroying_a_parameter_is_rejected() {
        rejects(
            &with_buf("fn drop_it(b: buffer<u8, 16> @RAM) { destroy b; } fn main() { }"),
            "cannot destroy parameter 'b'",
        );
    }

    // ---- borrows ---------------------------------------------------------

    #[test]
    fn using_the_owner_during_a_write_borrow_is_rejected() {
        rejects(
            &with_buf(
                "fn main() { buffer<u8, 16> x @RAM; buffer<u8, 16> y @RAM; let w = borrow x write; fir(x, y); release w; }",
            ),
            "entity 'x' is borrowed by 'w'",
        );
    }

    #[test]
    fn using_the_owner_during_a_read_borrow_is_rejected() {
        rejects(
            &with_buf(
                "fn main() { buffer<u8, 16> x @RAM; let r = borrow x read; peek(x); release r; }",
            ),
            "entity 'x' is borrowed by 'r'",
        );
    }

    #[test]
    fn access_through_the_borrow_handle_is_accepted() {
        accepts(&with_buf(
            "fn main() { buffer<u8, 16> x @RAM; buffer<u8, 16> y @RAM; let w = borrow x write; fir(y, w); release w; peek(x); }",
        ));
    }

    #[test]
    fn using_a_released_borrow_is_rejected() {
        rejects(
            &with_buf(
                "fn main() { buffer<u8, 16> x @RAM; let r = borrow x read; release r; peek(r); }",
            ),
            "borrow 'r' of 'x' was already released",
        );
    }

    #[test]
    fn borrowing_a_dma_owned_buffer_is_rejected() {
        rejects(
            &with_buf(
                "fn main() { buffer<u8, 16> x @DMA; let r = borrow x read; release r; sync(x); }",
            ),
            "entity 'x' is owned by @DMA; sync(x) before borrowing it",
        );
    }

    #[test]
    fn rejected_borrow_does_not_cascade_into_its_release() {
        match check(&with_buf(
            "fn main() { buffer<u8, 16> x @DMA; cap(x); let r = borrow x read; release r; sync(x); }",
        )) {
            Err(errs) => assert_eq!(errs.len(), 1, "{errs:?}"),
            Ok(()) => panic!("expected the borrow to be rejected"),
        }
    }

    #[test]
    fn borrowing_from_a_borrow_handle_is_rejected() {
        rejects(
            &with_buf(
                "fn main() { buffer<u8, 16> x @RAM; let r = borrow x read; let rr = borrow r read; release rr; release r; }",
            ),
            "'r' is itself a borrow of 'x'",
        );
    }

    #[test]
    fn moving_a_borrow_handle_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; let w = borrow x write; move w -> @DMA; release w; }"),
            "'w' is a borrow of 'x'; cannot move a borrow handle",
        );
    }

    // ---- aliasing --------------------------------------------------------

    #[test]
    fn same_buffer_twice_in_one_call_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; fir(x, x); }"),
            "entity 'x' is passed to 'fir' more than once",
        );
    }

    #[test]
    fn same_write_borrow_twice_in_one_call_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; let w = borrow x write; fir(w, w); release w; }"),
            "entity 'x' is passed to 'fir' more than once",
        );
    }

    #[test]
    fn two_read_borrows_of_one_buffer_may_alias() {
        accepts(&with_buf(
            "extern fn cmp2(a: read buffer<u8, 16> @RAM, b: read buffer<u8, 16> @RAM); fn main() { buffer<u8, 16> x @RAM; let a = borrow x read; let b = borrow x read; cmp2(a, b); release a; release b; }",
        ));
        // Read-only data cannot go to `fir`, which doesn't say it only reads.
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; let a = borrow x read; let b = borrow x read; fir(a, b); release a; release b; }"),
            "'fir' doesn't declare whether it writes its parameter",
        );
    }

    // ---- names -----------------------------------------------------------

    #[test]
    fn redeclaring_a_name_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; buffer<u8, 16> x @RAM; }"),
            "'x' is already declared in fn 'main'",
        );
    }

    #[test]
    fn same_name_in_sibling_branches_is_accepted() {
        accepts(&with_buf(
            "fn main() { let f: bool = get_flag(); if f { buffer<u8, 16> t @RAM; peek(t); } else { buffer<u8, 16> t @RAM; peek(t); } }",
        ));
    }

    #[test]
    fn unknown_name_in_a_condition_is_rejected() {
        rejects(
            &with_buf("fn main() { if nope { } }"),
            "unknown name 'nope'",
        );
        rejects(
            &with_buf("fn main() { while ghost < 3 { } }"),
            "unknown name 'ghost'",
        );
    }

    #[test]
    fn entity_used_as_a_scalar_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; if x == 0 { } }"),
            "'x' is an entity, not a scalar value",
        );
    }

    #[test]
    fn wrong_argument_count_is_rejected() {
        rejects(
            &with_buf("fn main() { buffer<u8, 16> x @RAM; fir(x); }"),
            "'fir' expects 2 arguments, found 1",
        );
    }

    #[test]
    fn duplicate_function_is_rejected() {
        rejects(
            "fn main() { } fn main() { }",
            "function 'main' is declared more than once",
        );
    }

    #[test]
    fn nested_branch_error_does_not_cascade_to_the_outer_join() {
        let errs = check(&with_buf(
            "fn main() { let f: bool = get_flag(); let g: bool = get_flag(); buffer<u8, 16> x @RAM; if f { peek(x); } else { if g { move x -> @DMA; } else { peek(x); } } }",
        ))
        .unwrap_err();
        assert_eq!(errs.len(), 1, "{errs:?}");
    }

    #[test]
    fn identical_errors_are_reported_once() {
        let errs = check(&with_buf(
            "fn main() { buffer<u8, 16> x @DMA; fir(x, x); sync(x); }",
        ))
        .unwrap_err();
        let msgs: Vec<&str> = errs.iter().map(|e| e.message.as_str()).collect();
        let mut dedup = msgs.clone();
        dedup.dedup();
        assert_eq!(msgs, dedup);
    }
}

/// v1.1: every error carries the location of the offending statement, the
/// entity it is about, and notes pointing at the cause.
#[cfg(test)]
mod location_tests {
    use super::*;

    fn errors(src: &str) -> Vec<AnalysisError> {
        analyze(&phase_parser::parse(src).expect("fixture must parse")).unwrap_err()
    }

    fn line_of(span: Option<Span>) -> u32 {
        span.expect("error must have a span").line
    }

    const SRC: &str = "extern fn fir(in_: buffer<u8, 4> @RAM, out: buffer<u8, 4> @RAM);
extern fn cap(x: buffer<u8, 4> @DMA);
fn main() {
    buffer<u8, 4> raw @RAM;
    buffer<u8, 4> out @RAM;
    move raw -> @DMA;
    cap(raw);
    fir(raw, out);
    sync(raw);
}
";

    #[test]
    fn domain_error_points_at_the_call_and_notes_the_move() {
        let errs = errors(SRC);
        assert_eq!(errs.len(), 1, "{errs:?}");
        let e = &errs[0];
        assert_eq!(line_of(e.span), 8);
        assert_eq!(e.subject.as_deref(), Some("raw"));
        assert_eq!(e.notes.len(), 1);
        assert_eq!(e.notes[0].span.line, 6);
        assert!(e.notes[0].message.contains("moved to @DMA"));
    }

    #[test]
    fn statement_span_covers_the_whole_statement() {
        let e = &errors(SRC)[0];
        let span = e.span.unwrap();
        // `    fir(raw, out);` -- from `fir` to just past `;`
        assert_eq!((span.col, span.end_line, span.end_col), (5, 8, 19));
    }

    #[test]
    fn falling_off_the_end_is_reported_at_the_closing_brace() {
        let src = "extern fn cap(x: buffer<u8, 4> @DMA);
fn main() {
    buffer<u8, 4> raw @RAM;
    move raw -> @DMA;
    cap(raw);
}
";
        let e = &errors(src)[0];
        assert!(e
            .message
            .contains("still owned by @DMA when 'main' returns"));
        assert_eq!(e.span.map(|s| (s.line, s.col)), Some((6, 1)));
        assert_eq!(e.notes[0].span.line, 4);
    }

    #[test]
    fn errors_inside_a_branch_point_at_the_inner_statement() {
        let src = "extern fn peek(x: buffer<u8, 4> @RAM);
extern fn flag() -> bool;
fn main() {
    let f: bool = flag();
    buffer<u8, 4> x @RAM;
    if f {
        destroy x;
        peek(x);
    }
}
";
        let errs = errors(src);
        let use_after = errs
            .iter()
            .find(|e| e.message.contains("was destroyed"))
            .unwrap();
        assert_eq!(line_of(use_after.span), 8);
        assert_eq!(use_after.notes[0].span.line, 7);
        // ...while the join disagreement points at the `if` itself.
        let join = errs
            .iter()
            .find(|e| e.message.contains("destroyed-state"))
            .unwrap();
        assert_eq!(line_of(join.span), 6);
    }

    #[test]
    fn branch_disagreement_notes_each_path() {
        let src = "extern fn flag() -> bool;
fn main() {
    let f: bool = flag();
    buffer<u8, 4> x @RAM;
    if f {
        move x -> @DMA;
    } else {
        move x -> @DEVICE;
    }
}
";
        let e = errors(src)
            .into_iter()
            .find(|e| e.message.contains("disagrees on domain"))
            .unwrap();
        let note_lines: Vec<u32> = e.notes.iter().map(|n| n.span.line).collect();
        assert_eq!(note_lines, vec![6, 8]);
    }

    #[test]
    fn borrow_conflict_notes_the_existing_borrow() {
        let src = "fn main() {
    buffer<u8, 4> x @RAM;
    let a = borrow x write;
    let b = borrow x write;
    release a;
}
";
        let e = errors(src)
            .into_iter()
            .find(|e| e.message.contains("exclusive write borrow"))
            .unwrap();
        assert_eq!(line_of(e.span), 4);
        assert_eq!(e.notes[0].span.line, 3);
    }

    #[test]
    fn duplicate_function_points_at_both_names() {
        let e = &errors("fn main() { }\nfn main() { }\n")[0];
        assert_eq!(e.span.map(|s| (s.line, s.col)), Some((2, 4)));
        assert_eq!(e.notes[0].span.line, 1);
    }

    #[test]
    fn every_gallery_style_error_has_a_span() {
        // Exercise a spread of rules; none may come back location-less.
        let src = "state P { A -> B }
extern fn mk() -> P<A>;
extern fn step(p: P<A>) -> P<B>;
extern fn peek(x: buffer<u8, 4> @RAM);
fn main() {
    let p: P<B> = mk();
    let q = mk();
    let r = step(q);
    let s = step(q);
    buffer<u8, 4> x @RAM;
    buffer<u8, 4> x @RAM;
    let v = borrow x read;
    peek(x);
    if ghost { }
}
";
        for e in errors(src) {
            assert!(e.span.is_some(), "no span on: {}", e.message);
        }
    }
}

/// v1.1: `@FROM -> @TO` domain changes declared on parameters.
#[cfg(test)]
mod domain_change_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[track_caller]
    fn rejects(src: &str, needle: &str) -> AnalysisError {
        match check(src) {
            Ok(()) => panic!("expected an error containing {needle:?}, got Ok(())"),
            Err(errs) => errs
                .into_iter()
                .find(|e| e.message.contains(needle))
                .unwrap_or_else(|| panic!("no error containing {needle:?}")),
        }
    }

    #[track_caller]
    fn accepts(src: &str) {
        assert_eq!(check(src), Ok(()));
    }

    const SIGS: &str = r#"
        extern fn start(dst: buffer<u8, 16> @RAM -> @DMA);
        extern fn finish(dst: buffer<u8, 16> @DMA -> @RAM);
        extern fn play(src: buffer<u8, 16> @RAM -> @DEVICE);
        extern fn peek(x: buffer<u8, 16> @RAM);
        extern fn get_n() -> u32;
    "#;

    fn with_sigs(body: &str) -> String {
        format!("{SIGS}\n{body}")
    }

    #[test]
    fn extern_call_moves_the_argument_to_its_declared_domain() {
        let e = rejects(
            &with_sigs("fn main() { buffer<u8, 16> x @RAM; start(x); peek(x); sync(x); }"),
            "entity 'x' is in domain @DMA but 'peek' expects @RAM",
        );
        assert!(
            e.notes[0].message.contains("moved to @DMA by 'start'"),
            "{e:?}"
        );
    }

    #[test]
    fn start_then_sync_is_accepted() {
        accepts(&with_sigs(
            "fn main() { buffer<u8, 16> x @RAM; start(x); sync(x); peek(x); }",
        ));
    }

    #[test]
    fn start_then_extern_finish_is_accepted() {
        accepts(&with_sigs(
            "fn main() { buffer<u8, 16> x @RAM; start(x); finish(x); peek(x); }",
        ));
    }

    #[test]
    fn device_hand_off_must_also_be_synced_before_scope_exit() {
        rejects(
            &with_sigs("fn main() { buffer<u8, 16> x @RAM; play(x); }"),
            "still owned by @DEVICE when 'main' returns",
        );
    }

    #[test]
    fn phase_helper_keeping_its_promise_is_accepted() {
        accepts(&with_sigs(
            r#"
            fn wait(b: buffer<u8, 16> @DMA -> @RAM) { sync(b); }
            fn main() { buffer<u8, 16> x @RAM; start(x); wait(x); peek(x); }
            "#,
        ));
    }

    #[test]
    fn phase_helper_breaking_its_promise_is_rejected() {
        rejects(
            &with_sigs("fn wait(b: buffer<u8, 16> @DMA -> @RAM) { } fn main() { }"),
            "parameter 'b' must be in @RAM when 'wait' returns (its signature says @DMA -> @RAM), but it is in @DMA",
        );
    }

    #[test]
    fn phase_helper_breaking_its_promise_on_one_path_is_rejected() {
        rejects(
            &with_sigs(
                "fn wait(b: buffer<u8, 16> @DMA -> @RAM) { let n: u32 = get_n(); if n == 0 { return; } sync(b); } fn main() { }",
            ),
            "parameter 'b' must be in @RAM when 'wait' returns",
        );
    }

    #[test]
    fn phase_helper_can_start_a_transfer_for_its_caller() {
        accepts(&with_sigs(
            r#"
            fn arm(b: buffer<u8, 16> @RAM -> @DMA) { start(b); }
            fn main() { buffer<u8, 16> x @RAM; arm(x); sync(x); peek(x); }
            "#,
        ));
    }

    #[test]
    fn caller_view_follows_a_phase_helper_declared_later_in_the_file() {
        rejects(
            &with_sigs(
                r#"
                fn main() { buffer<u8, 16> x @RAM; arm(x); peek(x); sync(x); }
                fn arm(b: buffer<u8, 16> @RAM -> @DMA) { start(b); }
                "#,
            ),
            "entity 'x' is in domain @DMA but 'peek' expects @RAM",
        );
    }

    #[test]
    fn argument_in_the_wrong_starting_domain_is_rejected_without_a_transition() {
        // finish(x) takes x from @DMA to @RAM, then start(x) hands it back:
        accepts(&with_sigs(
            "fn main() { buffer<u8, 16> x @DMA; finish(x); start(x); sync(x); }",
        ));
        // ...but starting a transfer on a buffer the engine already owns is
        // the domain error, at the call:
        let e = rejects(
            &with_sigs("fn main() { buffer<u8, 16> x @DMA; start(x); sync(x); }"),
            "entity 'x' is in domain @DMA but 'start' expects @RAM",
        );
        assert_eq!(e.span.map(|s| s.line), Some(8)); // the body line
    }

    #[test]
    fn a_borrow_cannot_be_handed_to_the_hardware() {
        rejects(
            &with_sigs(
                "fn main() { buffer<u8, 16> x @RAM; let w = borrow x write; start(w); release w; }",
            ),
            "'start' moves its argument to @DMA, but 'w' is only a borrow",
        );
    }

    #[test]
    fn illegal_declared_transition_is_rejected() {
        let e = rejects(
            "extern fn teleport(b: buffer<u8, 16> @DMA -> @DEVICE); fn main() { }",
            "no legal transition from @DMA to @DEVICE in the signature of 'teleport'",
        );
        assert_eq!(e.span.map(|s| (s.line, s.col)), Some((1, 20)));
    }

    #[test]
    fn no_op_declared_transition_is_rejected() {
        rejects(
            "extern fn f(b: buffer<u8, 16> @RAM -> @RAM); fn main() { }",
            "declares @RAM -> @RAM, which changes nothing",
        );
    }

    #[test]
    fn transfer_started_and_finished_inside_a_loop_is_accepted() {
        accepts(&with_sigs(
            "fn main() { let n: u32 = 0; buffer<u8, 16> x @RAM; while n < 4 { start(x); finish(x); peek(x); n = n + 1; } }",
        ));
    }

    #[test]
    fn transfer_started_inside_a_loop_but_not_finished_is_rejected() {
        rejects(
            &with_sigs(
                "fn main() { let n: u32 = 0; buffer<u8, 16> x @RAM; while n < 4 { start(x); n = n + 1; } sync(x); }",
            ),
            "a `while` body must leave every entity exactly as it found it",
        );
    }
}

/// v1.2: `read` / `write` access modes on parameters.
#[cfg(test)]
mod access_mode_tests {
    use super::*;

    fn full(src: &str) -> Report {
        analyze_full(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[track_caller]
    fn rejects(src: &str, needle: &str) {
        let r = full(src);
        assert!(
            r.errors.iter().any(|e| e.message.contains(needle)),
            "expected an error containing {needle:?}, got {:?}",
            r.errors
        );
    }

    #[track_caller]
    fn accepts(src: &str) {
        let r = full(src);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            r.warnings.is_empty(),
            "unexpected warnings: {:?}",
            r.warnings
        );
    }

    const SIGS: &str = r#"
        extern fn fir(in_: read buffer<u8, 8> @RAM, out: write buffer<u8, 8> @RAM);
        extern fn cmp(a: read buffer<u8, 8> @RAM, b: read buffer<u8, 8> @RAM);
        extern fn look(x: read buffer<u8, 8> @RAM);
        extern fn poke(x: write buffer<u8, 8> @RAM);
        extern fn legacy(x: buffer<u8, 8> @RAM);
    "#;

    fn with(body: &str) -> String {
        format!("{SIGS}\n{body}")
    }

    // ---- aliasing ----------------------------------------------------------

    #[test]
    fn same_buffer_to_two_read_params_is_accepted() {
        accepts(&with("fn main() { buffer<u8, 8> x @RAM; cmp(x, x); }"));
    }

    #[test]
    fn same_buffer_as_read_input_and_write_output_is_rejected() {
        rejects(
            &with("fn main() { buffer<u8, 8> x @RAM; fir(x, x); }"),
            "entity 'x' is passed to 'fir' more than once",
        );
    }

    // ---- owner access during borrows -------------------------------------

    #[test]
    fn owner_may_be_read_while_only_read_borrows_are_live() {
        accepts(&with(
            "fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; look(x); look(r); release r; }",
        ));
    }

    #[test]
    fn owner_may_not_be_written_while_read_borrowed() {
        rejects(
            &with("fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; poke(x); release r; }"),
            "entity 'x' is borrowed by 'r'",
        );
    }

    #[test]
    fn owner_may_not_even_be_read_while_write_borrowed() {
        rejects(
            &with(
                "fn main() { buffer<u8, 8> x @RAM; let w = borrow x write; look(x); release w; }",
            ),
            "entity 'x' is borrowed by 'w'",
        );
    }

    // ---- read-only arguments ---------------------------------------------

    #[test]
    fn read_borrow_to_a_write_param_is_rejected() {
        rejects(
            &with("fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; poke(r); release r; }"),
            "'poke' may write its parameter 'x', but 'r' is read-only",
        );
    }

    #[test]
    fn read_only_data_to_an_unannotated_param_is_an_error_since_2_0() {
        rejects(
            &with(
                "fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; legacy(r); release r; }",
            ),
            "'legacy' doesn't declare whether it writes its parameter 'x', but 'r' is read-only",
        );
        rejects(
            &with("fn f(b: read buffer<u8, 8> @RAM) { legacy(b); } fn main() { }"),
            "'legacy' doesn't declare whether it writes its parameter 'x', but 'b' is read-only",
        );
        // Data the caller may write can still go to an unannotated
        // parameter, which is assumed to write.
        accepts(&with("fn main() { buffer<u8, 8> x @RAM; legacy(x); let w = borrow x write; legacy(w); release w; }"));
    }

    #[test]
    fn write_borrow_may_go_anywhere() {
        accepts(&with(
            "fn main() { buffer<u8, 8> x @RAM; let w = borrow x write; poke(w); look(w); release w; }",
        ));
    }

    // ---- `read` parameters inside PHASE functions -------------------------

    #[test]
    fn read_param_can_be_read_and_read_borrowed() {
        accepts(&with(
            "fn show(b: read buffer<u8, 8> @RAM) { look(b); let r = borrow b read; cmp(b, r); release r; } fn main() { }",
        ));
    }

    #[test]
    fn read_param_cannot_be_written() {
        rejects(
            &with("fn f(b: read buffer<u8, 8> @RAM) { poke(b); } fn main() { }"),
            "'poke' may write its parameter 'x', but 'b' is read-only",
        );
        rejects(
            &with("fn f(b: read buffer<u8, 8> @RAM) { legacy(b); } fn main() { }"),
            "'b' is read-only",
        );
    }

    #[test]
    fn read_param_cannot_be_moved_destroyed_or_write_borrowed() {
        rejects(
            &with("fn f(b: read buffer<u8, 8> @RAM) { move b -> @DMA; } fn main() { }"),
            "'b' is a `read` parameter of 'f'; cannot move it",
        );
        rejects(
            &with("fn f(b: read buffer<u8, 8> @RAM) { destroy b; } fn main() { }"),
            "'b' is a `read` parameter of 'f'; cannot destroy it",
        );
        rejects(
            &with("fn f(b: read buffer<u8, 8> @RAM) { let w = borrow b write; release w; } fn main() { }"),
            "it can only be borrowed `read`",
        );
    }

    #[test]
    fn caller_may_pass_an_owner_to_a_read_param_during_a_read_borrow() {
        accepts(&with(
            r#"
            fn show(b: read buffer<u8, 8> @RAM) { look(b); }
            fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; show(x); show(r); release r; }
            "#,
        ));
    }

    // ---- typestate: `read` inspects without consuming ----------------------

    const PKT: &str = r#"
        state Packet { Received -> Validated }
        extern fn recv() -> Packet<Received>;
        extern fn check(p: Packet<Received>) -> Packet<Validated>;
        extern fn inspect(p: read Packet<Validated>);
        extern fn handle(p: Packet<Validated>);
    "#;

    #[test]
    fn read_typestate_param_inspects_without_consuming() {
        accepts(&format!(
            "{PKT} fn main() {{ let p = recv(); let v = check(p); inspect(v); inspect(v); handle(v); }}"
        ));
    }

    #[test]
    fn read_typestate_param_still_checks_the_state() {
        rejects(
            &format!(
                "{PKT} fn main() {{ let p = recv(); inspect(p); let v = check(p); handle(v); }}"
            ),
            "expected state Validated, found Received",
        );
    }

    #[test]
    fn consumed_value_cannot_be_inspected() {
        rejects(
            &format!(
                "{PKT} fn main() {{ let p = recv(); let v = check(p); handle(v); inspect(v); }}"
            ),
            "value 'v' was already consumed",
        );
    }

    // ---- signatures ------------------------------------------------------

    #[test]
    fn read_with_a_domain_change_is_rejected() {
        rejects(
            "extern fn start(b: read buffer<u8, 8> @RAM -> @DMA); fn main() { }",
            "is declared `read` but also hands the buffer to hardware",
        );
    }

    #[test]
    fn write_with_a_domain_change_is_fine() {
        accepts(
            "extern fn start(b: write buffer<u8, 8> @RAM -> @DMA); fn main() { buffer<u8, 8> x @RAM; start(x); sync(x); }",
        );
    }

    #[test]
    fn access_mode_on_a_scalar_is_rejected() {
        rejects(
            "extern fn f(n: read u32); fn main() { }",
            "access modes only apply to buffers and entities",
        );
    }

    #[test]
    fn field_read_is_allowed_during_a_read_borrow() {
        accepts(
            "entity S : @RAM { v: u32 } fn main() { let s: S @RAM = S { v: 1 }; let r = borrow s read; if s.v == 1 { } release r; }",
        );
    }
}

/// v1.2: `swap a, b;` and ping-pong double buffering.
#[cfg(test)]
mod swap_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[track_caller]
    fn rejects(src: &str, needle: &str) {
        match check(src) {
            Ok(()) => panic!("expected an error containing {needle:?}, got Ok(())"),
            Err(errs) => assert!(
                errs.iter().any(|e| e.message.contains(needle)),
                "expected {needle:?}, got {errs:?}"
            ),
        }
    }

    #[track_caller]
    fn accepts(src: &str) {
        assert_eq!(check(src), Ok(()));
    }

    const SIGS: &str = r#"
        extern fn start(dst: write buffer<u8, 8> @RAM -> @DMA);
        extern fn use_it(x: read buffer<u8, 8> @RAM);
        extern fn get_n() -> u32;
        fn wait(b: buffer<u8, 8> @DMA -> @RAM) { sync(b); }
    "#;

    fn with(body: &str) -> String {
        format!("{SIGS}\n{body}")
    }

    const PING_PONG: &str = r#"
        fn main() {
            buffer<u8, 8> ping @RAM;
            buffer<u8, 8> pong @RAM;
            let n: u32 = 0;
            start(ping);
            while n < 4 {
                wait(ping);
                start(pong);
                use_it(ping);
                swap ping, pong;
                n = n + 1;
            }
            wait(ping);
            use_it(ping);
        }
    "#;

    #[test]
    fn ping_pong_loop_is_accepted() {
        accepts(&with(PING_PONG));
    }

    #[test]
    fn swap_exchanges_domains() {
        // After the swap `a` is the one in flight.
        rejects(
            &with("fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; start(b); swap a, b; use_it(a); sync(a); }"),
            "entity 'a' is in domain @DMA but 'use_it' expects @RAM",
        );
        accepts(&with(
            "fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; start(b); swap a, b; use_it(b); sync(a); }",
        ));
    }

    #[test]
    fn swap_moves_the_history_too() {
        let errs = check(&with(
            "fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; start(b); swap a, b; use_it(a); sync(a); }",
        ))
        .unwrap_err();
        assert!(
            errs[0].notes[0]
                .message
                .contains("'a' is @DMA after `swap a, b`"),
            "{errs:?}"
        );
    }

    #[test]
    fn forgetting_the_swap_breaks_the_loop_invariant() {
        rejects(
            &with(&PING_PONG.replace("swap ping, pong;", "")),
            "entity 'ping' is @RAM at the end of the `while` body but @DMA when the loop is entered",
        );
    }

    #[test]
    fn swap_with_a_live_borrow_is_rejected() {
        rejects(
            &with("fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; let r = borrow b read; swap a, b; release r; }"),
            "entity 'b' has a live borrow; cannot swap",
        );
    }

    #[test]
    fn swap_of_different_buffer_types_is_rejected() {
        rejects(
            "fn main() { buffer<u8, 8> a @RAM; buffer<u8, 16> b @RAM; swap a, b; }",
            "cannot swap 'a' (buffer<u8, 8>) with 'b' (buffer<u8, 16>): the buffer types differ",
        );
    }

    #[test]
    fn swap_of_non_buffers_parameters_or_itself_is_rejected() {
        rejects(
            "entity S : @RAM { v: u32 } fn main() { let a: S @RAM = S { v: 1 }; let b: S @RAM = S { v: 2 }; swap a, b; }",
            "'a' is not a buffer; only buffers can be swapped",
        );
        rejects(
            "fn f(p: buffer<u8, 8> @RAM) { buffer<u8, 8> q @RAM; swap p, q; } fn main() { }",
            "cannot swap parameter 'p'",
        );
        rejects(
            "fn main() { buffer<u8, 8> a @RAM; swap a, a; }",
            "cannot swap 'a' with itself",
        );
    }

    #[test]
    fn swap_of_destroyed_or_borrow_handles_is_rejected() {
        rejects(
            "fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; destroy a; swap a, b; }",
            "entity 'a' was destroyed; cannot swap",
        );
        rejects(
            "fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; let r = borrow a read; swap r, b; release r; }",
            "'r' is a borrow of 'a'; cannot swap a borrow handle",
        );
    }

    #[test]
    fn swap_on_one_branch_only_disagrees_at_the_join() {
        rejects(
            &with("fn main() { let n: u32 = get_n(); buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; start(b); if n == 1 { swap a, b; } sync(b); }"),
            "disagrees on domain across incoming branches",
        );
    }
}

/// v1.3: names declared inside a block are checked when the block ends.
#[cfg(test)]
mod block_scope_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    const SIGS: &str = r#"
        extern fn flip() -> bool;
        extern fn count() -> u32;
        extern fn start(x: write buffer<u8, 8> @RAM -> @DMA);
    "#;

    #[test]
    fn hardware_owned_buffer_leaving_an_if_block_is_rejected() {
        let errs = check(&format!(
            "{SIGS} fn main() {{ let c: bool = flip(); if c {{ buffer<u8, 8> t @RAM; start(t); }} }}"
        ))
        .unwrap_err();
        assert!(
            errs[0]
                .message
                .contains("entity 't' is still owned by @DMA when its block ends"),
            "{errs:?}"
        );
    }

    #[test]
    fn hardware_owned_buffer_leaving_a_loop_body_is_rejected() {
        let errs = check(&format!(
            "{SIGS} fn main() {{ let n: u32 = 0; let l: u32 = count(); while n < l {{ buffer<u8, 8> t @DMA; n = n + 1; }} }}"
        ))
        .unwrap_err();
        assert!(errs[0].message.contains("when its block ends"), "{errs:?}");
    }

    #[test]
    fn synced_block_local_and_returning_branches_are_fine() {
        assert_eq!(
            check(&format!(
                "{SIGS} fn main() {{ let c: bool = flip(); if c {{ buffer<u8, 8> t @RAM; start(t); sync(t); }} }}"
            )),
            Ok(())
        );
        // A branch that returns is checked by the return, not the block end.
        let errs = check(&format!(
            "{SIGS} fn main() {{ let c: bool = flip(); if c {{ buffer<u8, 8> t @RAM; start(t); return; }} }}"
        ))
        .unwrap_err();
        assert!(errs[0].message.contains("when 'main' returns"), "{errs:?}");
    }
}

/// v1.3: `swap` only between buffers declared in the same block.
#[cfg(test)]
mod swap_scope_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[test]
    fn swap_with_an_inner_block_buffer_is_rejected() {
        let errs = check(
            "extern fn flip() -> bool; fn main() { buffer<u8, 8> outer @RAM; let c: bool = flip(); if c { buffer<u8, 8> inner @RAM; swap outer, inner; } }",
        )
        .unwrap_err();
        assert!(
            errs[0].message.contains(
                "cannot swap 'outer' with 'inner': 'inner' is declared in an inner block"
            ),
            "{errs:?}"
        );
    }

    #[test]
    fn swap_of_two_buffers_from_the_same_inner_block_is_fine() {
        assert_eq!(
            check(
                "extern fn flip() -> bool; fn main() { let c: bool = flip(); if c { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; swap a, b; } }",
            ),
            Ok(())
        );
    }

    #[test]
    fn swap_of_outer_buffers_inside_a_block_is_fine() {
        // Both storages outlive the block, so swapping them inside it is safe.
        assert_eq!(
            check(
                "extern fn flip() -> bool; fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; let c: bool = flip(); if c { swap a, b; } }",
            ),
            Ok(())
        );
    }
}

/// v1.3: buffer types are checked at call sites; `buffer<T, _>` parameters.
#[cfg(test)]
mod buffer_type_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[track_caller]
    fn rejects(src: &str, needle: &str) {
        match check(src) {
            Ok(()) => panic!("expected an error containing {needle:?}, got Ok(())"),
            Err(errs) => assert!(
                errs.iter().any(|e| e.message.contains(needle)),
                "expected {needle:?}, got {errs:?}"
            ),
        }
    }

    #[test]
    fn length_mismatch_is_rejected() {
        rejects(
            "extern fn f(x: write buffer<u8, 16> @RAM); fn main() { buffer<u8, 4> b @RAM; f(b); }",
            "'f' expects buffer<u8, 16> for 'x', but 'b' is buffer<u8, 4>",
        );
    }

    #[test]
    fn element_type_mismatch_is_rejected() {
        rejects(
            "extern fn f(x: write buffer<f32, 4> @RAM); fn main() { buffer<u8, 4> b @RAM; f(b); }",
            "'f' expects buffer<f32, 4> for 'x', but 'b' is buffer<u8, 4>",
        );
    }

    #[test]
    fn a_borrow_carries_its_targets_type() {
        rejects(
            "extern fn f(x: read buffer<u8, 16> @RAM); fn main() { buffer<u8, 4> b @RAM; let r = borrow b read; f(r); release r; }",
            "but 'r' is buffer<u8, 4>",
        );
    }

    #[test]
    fn scalar_or_entity_where_a_buffer_is_expected_is_rejected() {
        rejects(
            "extern fn f(x: read buffer<u8, 4> @RAM); fn main() { let n: u32 = 1; f(n); }",
            "but 'n' is u32, not a buffer",
        );
        rejects(
            "entity S : @RAM { v: u32 } extern fn g(s: S @RAM); fn main() { buffer<u8, 4> b @RAM; g(b); }",
            "'g' expects S for 's', but 'b' is a buffer (buffer<u8, 4>)",
        );
    }

    #[test]
    fn entity_base_type_mismatch_is_rejected() {
        rejects(
            "entity A : @RAM { v: u32 } entity B : @RAM { v: u32 } extern fn g(a: A @RAM); fn main() { let b: B @RAM = B { v: 1 }; g(b); }",
            "'g' expects A for 'a', but 'b' is B",
        );
    }

    #[test]
    fn generic_length_parameter_accepts_any_length() {
        assert_eq!(
            check(
                "extern fn f(x: read buffer<u8, _> @RAM); fn main() { buffer<u8, 4> a @RAM; buffer<u8, 64> b @RAM; f(a); f(b); }"
            ),
            Ok(())
        );
    }

    #[test]
    fn generic_length_flows_only_into_generic_parameters() {
        assert_eq!(
            check(
                "extern fn f(x: read buffer<u8, _> @RAM); fn g(b: read buffer<u8, _> @RAM) { f(b); } fn main() { buffer<u8, 4> a @RAM; g(a); }"
            ),
            Ok(())
        );
        rejects(
            "extern fn f(x: read buffer<u8, 8> @RAM); fn g(b: read buffer<u8, _> @RAM) { f(b); } fn main() { }",
            "but 'b' is buffer<u8, _> (a run-time length, which may not be 8)",
        );
    }

    #[test]
    fn generic_length_still_checks_the_element_type() {
        rejects(
            "extern fn f(x: read buffer<f32, _> @RAM); fn main() { buffer<u8, 4> a @RAM; f(a); }",
            "'f' expects buffer<f32, _> for 'x', but 'a' is buffer<u8, 4>",
        );
    }

    #[test]
    fn underscore_length_outside_parameters_is_rejected() {
        rejects(
            "fn main() { buffer<u8, _> b @RAM; }",
            "'b' needs a concrete length",
        );
        rejects(
            "entity S : @RAM { data: buffer<u8, _> } fn main() { }",
            "field 'data' of entity 'S' has length `_`",
        );
        rejects(
            "extern fn mk() -> buffer<u8, _>; fn main() { }",
            "'mk' returns a buffer of length `_`",
        );
    }
}

/// v1.4: `rotate`, and `split`/`join` for half-buffer (circular) DMA.
#[cfg(test)]
mod ring_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    #[track_caller]
    fn rejects(src: &str, needle: &str) {
        match check(src) {
            Ok(()) => panic!("expected an error containing {needle:?}, got Ok(())"),
            Err(errs) => assert!(
                errs.iter().any(|e| e.message.contains(needle)),
                "expected {needle:?}, got {errs:?}"
            ),
        }
    }

    #[track_caller]
    fn accepts(src: &str) {
        assert_eq!(check(src), Ok(()));
    }

    const SIGS: &str = r#"
        extern fn start(dst: write buffer<u8, 8> @RAM -> @DMA);
        extern fn start4(dst: write buffer<u8, 4> @RAM -> @DMA);
        extern fn wait4(dst: buffer<u8, 4> @DMA -> @RAM);
        extern fn look4(x: read buffer<u8, 4> @RAM);
        extern fn look(x: read buffer<u8, 8> @RAM);
        extern fn get_n() -> u32;
        fn wait(b: buffer<u8, 8> @DMA -> @RAM) { sync(b); }
    "#;

    fn with(body: &str) -> String {
        format!("{SIGS}\n{body}")
    }

    // ---- rotate ----------------------------------------------------------

    #[test]
    fn triple_buffer_loop_is_accepted() {
        accepts(&with(
            r#"fn main() {
                buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; buffer<u8, 8> c @RAM;
                let n: u32 = 0;
                start(a); start(b);
                while n < 4 { wait(a); start(c); look(a); rotate a, b, c; n = n + 1; }
                wait(a); wait(b);
            }"#,
        ));
    }

    #[test]
    fn rotate_moves_each_name_to_the_next_buffer() {
        // after `rotate a, b, c`: a has b's (in flight), c has a's (free)
        accepts(&with(
            "fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; buffer<u8, 8> c @RAM; start(b); rotate a, b, c; look(c); look(b); wait(a); }",
        ));
        rejects(
            &with("fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; buffer<u8, 8> c @RAM; start(b); rotate a, b, c; look(a); wait(a); }"),
            "entity 'a' is in domain @DMA but 'look' expects @RAM",
        );
    }

    #[test]
    fn rotate_needs_live_unborrowed_same_type_same_block_buffers() {
        rejects(
            &with("fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; buffer<u8, 4> c @RAM; rotate a, b, c; }"),
            "cannot rotate 'a' (buffer<u8, 8>) with 'c' (buffer<u8, 4>): the buffer types differ",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; let r = borrow b read; rotate a, b; release r; }"),
            "entity 'b' has a live borrow; cannot rotate",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; rotate a, b, a; }"),
            "cannot rotate 'a' with itself",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> a @RAM; let f: u32 = get_n(); if f == 1 { buffer<u8, 8> t @RAM; rotate a, t; } }"),
            "'t' is declared in an inner block",
        );
        rejects(
            &with(
                "fn f(p: buffer<u8, 8> @RAM) { buffer<u8, 8> q @RAM; rotate q, p; } fn main() { }",
            ),
            "cannot rotate parameter 'p'",
        );
    }

    // ---- split / join ------------------------------------------------------

    const CIRCULAR: &str = r#"
        fn main() {
            buffer<u8, 8> ring @RAM;
            let n: u32 = 0;
            split ring into lo, hi;
            start4(lo); start4(hi);
            while n < 3 {
                wait4(lo); look4(lo); start4(lo);
                wait4(hi); look4(hi); start4(hi);
                n = n + 1;
            }
            wait4(lo); wait4(hi);
            join lo, hi into ring;
            look(ring);
        }
    "#;

    #[test]
    fn circular_half_buffer_loop_is_accepted() {
        accepts(&with(CIRCULAR));
    }

    #[test]
    fn the_whole_buffer_is_unusable_while_split() {
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; look(ring); join lo, hi into ring; }"),
            "'ring' is split into lo, hi; join them back first (cannot use as argument to 'look')",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; move ring -> @DMA; join lo, hi into ring; }"),
            "join them back first (cannot move it)",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; let r = borrow ring read; release r; join lo, hi into ring; }"),
            "join them back first (cannot borrow it)",
        );
    }

    #[test]
    fn parts_have_the_parent_domain_and_a_fraction_of_its_length() {
        rejects(
            &with("fn main() { buffer<u8, 8> ring @DMA; split ring into lo, hi; look4(lo); wait4(lo); wait4(hi); join lo, hi into ring; }"),
            "entity 'lo' is in domain @DMA but 'look4' expects @RAM",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; look(lo); join lo, hi into ring; }"),
            "'look' expects buffer<u8, 8> for 'x', but 'lo' is buffer<u8, 4>",
        );
    }

    #[test]
    fn join_requires_the_same_parts_in_order_in_one_domain() {
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; join hi, lo into ring; }"),
            "'ring' was split into lo, hi; join exactly those parts, in that order",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; start4(hi); join lo, hi into ring; wait4(hi); }"),
            "cannot join 'lo' (@RAM) and 'hi' (@DMA) into 'ring'",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; swap lo, hi; join lo, hi into ring; }"),
            "'lo' no longer holds part 0 of 'ring'",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; join ring, ring into ring; }"),
            "cannot join into 'ring': it is not split",
        );
    }

    #[test]
    fn swapping_parts_back_restores_joinability() {
        accepts(&with(
            "fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; swap lo, hi; swap lo, hi; join lo, hi into ring; }",
        ));
    }

    #[test]
    fn split_needs_a_fixed_divisible_owned_local() {
        rejects(
            "fn main() { buffer<u8, 9> ring @RAM; split ring into a, b; }",
            "cannot split 'ring' (buffer<u8, 9>) into 2 equal parts",
        );
        rejects(
            "fn f(r: buffer<u8, 8> @RAM) { split r into a, b; join a, b into r; } fn main() { }",
            "cannot split parameter 'r'",
        );
        rejects(
            "fn f(r: buffer<u8, _> @RAM) { buffer<u8, 8> x @RAM; } fn main() { buffer<u8, 8> s @RAM; let w = borrow s write; split s into a, b; release w; }",
            "entity 's' has a live borrow; cannot split",
        );
        rejects(
            "fn main() { buffer<u8, 8> ring @RAM; split ring into a, b; split ring into c, d; }",
            "join them back first (cannot split it)",
        );
    }

    #[test]
    fn a_part_cannot_be_destroyed_on_its_own() {
        rejects(
            "fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; destroy lo; }",
            "cannot destroy 'lo' on its own: it is part of 'ring'",
        );
    }

    #[test]
    fn unjoined_parts_carry_the_exit_and_block_end_obligations() {
        // never joined, but both halves back with the CPU: fine
        accepts(&with(
            "fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; start4(lo); wait4(lo); }",
        ));
        // a half left with the engine at the end of main
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; split ring into lo, hi; start4(lo); }"),
            "entity 'lo' is still owned by @DMA when 'main' returns",
        );
        // ...or at the end of the block the split happened in
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; let n: u32 = get_n(); if n == 1 { split ring into lo, hi; start4(hi); } }"),
            "entity 'hi' is still owned by @DMA when its block ends",
        );
    }

    #[test]
    fn split_on_one_path_or_unjoined_in_a_loop_is_rejected() {
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; let n: u32 = get_n(); if n == 1 { split ring into lo, hi; } look(ring); }"),
            "entity 'ring' is split on one path but not the other",
        );
        rejects(
            &with("fn main() { buffer<u8, 8> ring @RAM; let n: u32 = 0; while n < 2 { split ring into lo, hi; n = n + 1; } }"),
            "entity 'ring' is split on one path but not the other; a `while` body must leave every entity exactly as it found it",
        );
    }
}

#[cfg(test)]
mod device_tests {
    use super::*;

    fn check(src: &str) -> Result<(), Vec<AnalysisError>> {
        analyze(&phase_parser::parse(src).expect("fixture must parse"))
    }

    fn rejects(src: &str, needle: &str) {
        let errs = check(src).expect_err("expected a rejection");
        assert!(
            errs.iter().any(|e| e.message.contains(needle)),
            "no error containing {needle:?} in {:#?}",
            errs.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    const UART: &str = r#"
        device Uart0 @ 0x40004000 {
            DATA:  read write u32 @ 0x00;
            STATE: read u32 @ 0x04 { TXFULL: bit 0, RXFULL: bit 1 }
            CTRL:  read write u32 @ 0x08 = 0 { TX_EN: bit 0, MODE: bits 4..=6 }
            KICK:  write u8 @ 0x0C { GO: bit 0 }
        }
    "#;

    fn with(body: &str) -> String {
        format!("{UART}\n{body}")
    }

    #[test]
    fn reads_writes_and_fields_are_accepted() {
        check(&with(
            "fn send(c: u32) { while volatile_read(Uart0.STATE.TXFULL) == 1 { } volatile_write(Uart0.DATA, c); }
             fn main() {
                 volatile_write(Uart0.CTRL.MODE, 7);
                 volatile_write(Uart0.KICK, 255);
                 let s: u32 = volatile_read(Uart0.STATE);
                 send(s);
                 if volatile_read(Uart0.CTRL.TX_EN) == 0 { volatile_write(Uart0.CTRL.TX_EN, 1); }
             }",
        ))
        .unwrap();
    }

    #[test]
    fn unknown_registers_and_fields_are_named() {
        rejects(
            &with("fn main() { let s: u32 = volatile_read(Uart0.STATUS); }"),
            "device 'Uart0' has no register 'STATUS' (it has DATA, STATE, CTRL, KICK)",
        );
        rejects(
            &with("fn main() { let s: u32 = volatile_read(Uart0.STATE.TXFUL); }"),
            "register 'Uart0.STATE' has no field 'TXFUL' (it has TXFULL, RXFULL)",
        );
        rejects(
            &with("fn main() { let s: u32 = volatile_read(Uart0.DATA.LOW); }"),
            "register 'Uart0.DATA' has no field 'LOW' (it declares no fields)",
        );
    }

    #[test]
    fn access_modes_are_enforced() {
        rejects(
            &with("fn main() { volatile_write(Uart0.STATE, 0); }"),
            "'Uart0.STATE' is read-only; it cannot be written",
        );
        rejects(
            &with("fn main() { volatile_write(Uart0.STATE.TXFULL, 0); }"),
            "'Uart0.STATE' is read-only; it cannot be written",
        );
        rejects(
            &with("fn main() { let k: u32 = volatile_read(Uart0.KICK); }"),
            "'Uart0.KICK' is write-only; it cannot be read",
        );
        rejects(
            &with("fn main() { volatile_write(Uart0.KICK.GO, 1); }"),
            "cannot write field 'Uart0.KICK.GO': 'Uart0.KICK' is write-only",
        );
    }

    #[test]
    fn literal_values_must_fit() {
        rejects(
            &with("fn main() { volatile_write(Uart0.CTRL.MODE, 8); }"),
            "8 does not fit in 'Uart0.CTRL.MODE' (3 bits, at most 7)",
        );
        rejects(
            &with("fn main() { volatile_write(Uart0.CTRL.TX_EN, 2); }"),
            "2 does not fit in 'Uart0.CTRL.TX_EN' (1 bit, at most 1)",
        );
        rejects(
            &with("fn main() { volatile_write(Uart0.KICK, 256); }"),
            "256 does not fit in 'Uart0.KICK' (8 bits, at most 255)",
        );
    }

    #[test]
    fn registers_and_devices_are_not_values() {
        rejects(
            &with("fn main() { let s: u32 = 0; s = Uart0.STATE; }"),
            "'Uart0.STATE' is a device register; read it with volatile_read(Uart0.STATE)",
        );
        rejects(
            &with("fn main() { let s: u32 = 0; s = Uart0; }"),
            "'Uart0' is a device, not a value",
        );
        rejects(
            &with("fn main() { let Uart0: u32 = 0; }"),
            "'Uart0' is a device; choose another name",
        );
    }

    #[test]
    fn register_maps_are_validated() {
        rejects(
            "device D @ 0 { A: read u32 @ 0x02; } fn main() {}",
            "register 'D.A' is at offset 0x2, which is not aligned to its 4 bytes",
        );
        rejects(
            "device D @ 0 { A: read u32 @ 0x00; B: read u16 @ 0x02; } fn main() {}",
            "register 'D.B' (bytes 0x2..0x4) overlaps 'D.A'",
        );
        rejects(
            "device D @ 0 { A: read f32 @ 0; } fn main() {}",
            "register 'D.A' has type 'f32'; a register is u8, u16 or u32",
        );
        rejects(
            "device D @ 0 { A: read u8 @ 0 { X: bit 8 } } fn main() {}",
            "field 'D.A.X' uses bit 8, outside the 8-bit register",
        );
        rejects(
            "device D @ 0 { A: read u32 @ 0 { X: bits 5..=3 } } fn main() {}",
            "field 'D.A.X' is `bits 5..=3`; the low bit comes first",
        );
        rejects(
            "device D @ 0 { A: read u32 @ 0 { X: bits 0..=3, Y: bit 3 } } fn main() {}",
            "fields 'D.A.X' and 'D.A.Y' share bits",
        );
        rejects(
            "device D @ 0 { A: read u32 @ 0; A: read u32 @ 4; } fn main() {}",
            "device 'D' declares register 'A' more than once",
        );
        rejects(
            "device D @ 0 { A: read u8 @ 0 = 0x100; } fn main() {}",
            "reset value 0x100 does not fit in the 8-bit register 'D.A'",
        );
        rejects(
            "device D @ 0 { A: read u8 @ 0; } device D @ 4 { B: read u8 @ 0; } fn main() {}",
            "device 'D' is declared more than once",
        );
        rejects(
            "device main @ 0 { A: read u8 @ 0; } fn main() {}",
            "'main' is declared as a device and as a device type, function",
        );
    }

    const TYPED: &str = r#"
        device type Uart {
            DATA:  read write u32 @ 0x00;
            STATE: read u32 @ 0x04 { TXFULL: bit 0 }
        }
        device Uart0: Uart @ 0x40004000;
        device Uart1: Uart @ 0x40005000;
    "#;

    #[test]
    fn instances_have_their_types_registers() {
        check(&format!(
            "{TYPED} fn main() {{ volatile_write(Uart1.DATA, 65); let f: u32 = volatile_read(Uart0.STATE.TXFULL); }}"
        ))
        .unwrap();
        rejects(
            &format!("{TYPED} fn main() {{ volatile_write(Uart1.STATE, 0); }}"),
            "'Uart1.STATE' is read-only; it cannot be written",
        );
        rejects(
            &format!("{TYPED} fn main() {{ volatile_write(Uart0.CTRL, 0); }}"),
            "device 'Uart0' has no register 'CTRL' (it has DATA, STATE)",
        );
    }

    #[test]
    fn device_types_and_instances_are_validated() {
        rejects(
            "device U0: Uart @ 0x1000; fn main() {}",
            "device 'U0' is of type 'Uart', which is not declared (there are no device types)",
        );
        rejects(
            &format!("{TYPED} device U2: Uarts @ 0x1000; fn main() {{}}"),
            "device 'U2' is of type 'Uarts', which is not declared (device types: Uart)",
        );
        rejects(
            "device type T { A: read u32 @ 2; } fn main() {}",
            "register 'T.A' is at offset 0x2, which is not aligned to its 4 bytes",
        );
        rejects(
            "device type T { A: read u32 @ 0; } device type T { B: read u8 @ 0; } fn main() {}",
            "device type 'T' is declared more than once",
        );
        rejects(
            "device type T { A: read u32 @ 0; A: read u32 @ 4; } fn main() {}",
            "device type 'T' declares register 'A' more than once",
        );
        rejects(
            &format!("{TYPED} device Uart2: Uart @ 0x40006002; fn main() {{}}"),
            "device 'Uart2' is at 0x40006002, which is not aligned to its 4-byte registers",
        );
        rejects(
            "device type T { A: read u32 @ 0; } device T: T @ 0; fn main() {}",
            "'T' is declared as a device and as a device type",
        );
    }

    #[test]
    fn devices_may_not_overlap() {
        rejects(
            &format!("{TYPED} device Uart2: Uart @ 0x40005004; fn main() {{}}"),
            "devices 'Uart2' (0x40005004..0x4000500c) and 'Uart1' (0x40005000..0x40005008) overlap",
        );
        rejects(
            "device A @ 0x100 { R: read u32 @ 0x8; } device B @ 0x108 { S: read u8 @ 0; } fn main() {}",
            "devices 'B' (0x108..0x109) and 'A' (0x100..0x10c) overlap",
        );
        // Back to back is fine.
        check(&format!(
            "{TYPED} device Uart2: Uart @ 0x40005008; fn main() {{}}"
        ))
        .unwrap();
    }

    #[test]
    fn simulated_registers_still_work_alongside_devices() {
        check(&with(
            "fn main() { let s: u32 = volatile_read(LEGACY @MMIO); volatile_write(Uart0.DATA, s); }",
        ))
        .unwrap();
    }
}
