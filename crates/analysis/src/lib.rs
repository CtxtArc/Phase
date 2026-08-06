//! PHASE M2/M3: the domain/ownership/borrow analyzer, plus typestate.
//!
//! Scope (matches the roadmap in phase_specification.md): straight-line
//! function bodies only — the AST has no `if`/`while` yet, so control flow
//! and `ENTITY_PHI` merging (M4) simply don't arise here.
//!
//! M2 covers bug-gallery items #1-#4:
//!   1. use of a `@DMA`/`@DEVICE`-domain entity where a different domain
//!      was required, without an intervening `sync`
//!   2. `move`/`sync`/`destroy` while a borrow is live
//!   3. conflicting borrows (two writers, or a writer alongside readers)
//!   4. use of a destroyed entity
//! plus the domain-transition-table check from spec §2.4.
//!
//! M3 adds bug-gallery item #5: typestate checking. A `state` declaration
//! (spec §3.8) defines a named machine and its states; a value's type can
//! be parameterized by state (`Packet<Received>`), and calling a function
//! that requires one state with a value proven to be in a different state
//! is a compile error, the same way a domain mismatch is.

use phase_ast::{BorrowMode, Domain, Expr, FnDecl, Item, Program, Stmt, TypeExpr};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisError {
    pub message: String,
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

fn err(msg: impl Into<String>) -> AnalysisError {
    AnalysisError {
        message: msg.into(),
    }
}

/// `(machine name, state name)`, e.g. `("Packet", "Received")`.
type Typestate = (String, String);

/// name -> resolved per-parameter domain / typestate requirements, plus
/// the resolved return typestate (if the return type is stateful).
#[derive(Debug, Clone)]
struct FnSig {
    param_domains: Vec<Option<Domain>>,
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
    for item in &program.items {
        if let Item::State(s) = item {
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
        let _ = name;
        FnSig {
            param_domains: params.iter().map(|p| p.domain).collect(),
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

    for item in &program.items {
        if let Item::Fn(f) = item {
            let mut checker = FnChecker::new(&entity_domains, &fn_sigs, &state_machines);
            checker.check_fn(f, &mut errors);
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
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
}

struct FnChecker<'a> {
    entity_domains: &'a HashMap<&'a str, Option<Domain>>,
    fn_sigs: &'a HashMap<&'a str, FnSig>,
    state_machines: &'a HashMap<&'a str, HashSet<&'a str>>,
    locals: HashMap<String, VarState>,
    /// keyed by the *target* entity name being borrowed from
    borrows: HashMap<String, BorrowState>,
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

impl<'a> FnChecker<'a> {
    fn new(
        entity_domains: &'a HashMap<&'a str, Option<Domain>>,
        fn_sigs: &'a HashMap<&'a str, FnSig>,
        state_machines: &'a HashMap<&'a str, HashSet<&'a str>>,
    ) -> Self {
        FnChecker {
            entity_domains,
            fn_sigs,
            state_machines,
            locals: HashMap::new(),
            borrows: HashMap::new(),
        }
    }

    fn check_fn(&mut self, f: &FnDecl, errors: &mut Vec<AnalysisError>) {
        // Parameters are locals too: register them up front (using the
        // domain/typestate already resolved once for this function's
        // signature) so a body that uses a parameter directly -- not just
        // ones re-bound through a `let` -- is checked correctly.
        if let Some(sig) = self.fn_sigs.get(f.name.as_str()) {
            for (i, p) in f.params.iter().enumerate() {
                self.locals.insert(
                    p.name.clone(),
                    VarState {
                        domain: sig.param_domains.get(i).copied().flatten(),
                        typestate: sig.param_states.get(i).cloned().flatten(),
                        destroyed: false,
                        borrow_of: None,
                        released: false,
                    },
                );
            }
        }

        for stmt in &f.body.stmts {
            self.check_stmt(stmt, errors);
        }
        for stmt in &f.body.stmts {
            if let Stmt::Return(Some(Expr::Ident(name))) = stmt {
                self.check_return_value(name, f, errors);
            }
        }
    }

    fn check_return_value(&self, name: &str, f: &FnDecl, errors: &mut Vec<AnalysisError>) {
        let Some(sig) = self.fn_sigs.get(f.name.as_str()) else {
            return;
        };
        let Some(var) = self.locals.get(name) else {
            return; // unknown-ident-in-return is not this pass's job
        };
        if let (Some((req_machine, req_state)), Some((have_machine, have_state))) =
            (&sig.return_state, &var.typestate)
        {
            if req_machine != have_machine {
                errors.push(err(format!(
                    "entity '{name}' has typestate '{have_machine}' but function '{}' must return typestate '{req_machine}'",
                    f.name
                )));
            } else if req_state != have_state {
                errors.push(err(format!(
                    "expected state {req_state}, found {have_state}"
                )));
            }
        }
    }

    fn has_live_borrow(&self, name: &str) -> bool {
        self.borrows.get(name).map(|b| !b.is_empty()).unwrap_or(false)
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
                let Some(var) = self.locals.get(name) else {
                    errors.push(err(format!("unknown entity '{name}'")));
                    return;
                };
                if var.destroyed {
                    errors.push(err(format!(
                        "entity '{name}' was destroyed; cannot move"
                    )));
                    return;
                }
                let Some(from) = var.domain else { return };
                if self.has_live_borrow(name) {
                    errors.push(err(format!(
                        "entity '{name}' has a live borrow; cannot move"
                    )));
                    return;
                }
                if !legal_move(from, *to) {
                    errors.push(err(format!(
                        "no legal transition from @{} to @{} via move",
                        from.name(),
                        to.name()
                    )));
                    return;
                }
                self.locals.get_mut(name).unwrap().domain = Some(*to);
            }

            Stmt::Sync { name } => {
                let Some(var) = self.locals.get(name) else {
                    errors.push(err(format!("unknown entity '{name}'")));
                    return;
                };
                if var.destroyed {
                    errors.push(err(format!(
                        "entity '{name}' was destroyed; cannot sync"
                    )));
                    return;
                }
                let Some(from) = var.domain else { return };
                if self.has_live_borrow(name) {
                    errors.push(err(format!(
                        "entity '{name}' has a live borrow; cannot sync"
                    )));
                    return;
                }
                if !legal_sync_source(from) {
                    errors.push(err(format!(
                        "cannot sync entity '{name}': no legal sync transition from @{}",
                        from.name()
                    )));
                    return;
                }
                self.locals.get_mut(name).unwrap().domain = Some(Domain::Ram);
            }

            Stmt::Release { name } => {
                let Some(var) = self.locals.get(name) else {
                    errors.push(err(format!("unknown entity '{name}'")));
                    return;
                };
                let Some((target, mode)) = var.borrow_of.clone() else {
                    errors.push(err(format!("'{name}' is not an active borrow")));
                    return;
                };
                if var.released {
                    errors.push(err(format!("borrow '{name}' was already released")));
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
                self.locals.get_mut(name).unwrap().released = true;
            }

            Stmt::Destroy { name } => {
                let Some(var) = self.locals.get(name) else {
                    errors.push(err(format!("unknown entity '{name}'")));
                    return;
                };
                if var.destroyed {
                    errors.push(err(format!("entity '{name}' was already destroyed")));
                    return;
                }
                if self.has_live_borrow(name) {
                    errors.push(err(format!(
                        "entity '{name}' has a live borrow; cannot destroy"
                    )));
                    return;
                }
                self.locals.get_mut(name).unwrap().destroyed = true;
            }

            Stmt::Expr(e) => self.check_expr(e, errors),

            Stmt::Return(Some(e)) => self.check_expr(e, errors),
            Stmt::Return(None) => {}

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
                } else if var.domain.is_some() {
                    errors.push(err(format!(
                        "cannot assign to '{name}': it is domain-tracked (use move/sync instead)"
                    )));
                } else if var.destroyed {
                    errors.push(err(format!(
                        "cannot assign to '{name}': it was destroyed"
                    )));
                }
                self.check_expr(value, errors);
            }
        }
    }

    fn check_block(&mut self, block: &phase_ast::Block, errors: &mut Vec<AnalysisError>) {
        for stmt in &block.stmts {
            self.check_stmt(stmt, errors);
        }
    }

    /// M4 — `ENTITY_PHI`: check each branch starting from the *same*
    /// pre-branch state, then merge. A variable that already existed
    /// before the `if` must agree on domain/typestate/destroyed-ness on
    /// every incoming path, or the merge is rejected (bug-gallery #6).
    /// A variable introduced *inside* a branch is properly out of scope
    /// afterward — it's simply not carried into the merged state, since
    /// only pre-existing names are merged.
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

        self.check_block(then_block, errors);
        let then_locals = std::mem::replace(&mut self.locals, base_locals.clone());
        let then_borrows = std::mem::replace(&mut self.borrows, base_borrows.clone());

        if let Some(eb) = else_block {
            self.check_block(eb, errors);
        }
        let else_locals = std::mem::replace(&mut self.locals, base_locals.clone());
        let else_borrows = std::mem::replace(&mut self.borrows, base_borrows.clone());

        self.merge_branches(
            &base_locals,
            &base_borrows,
            &then_locals,
            &then_borrows,
            &else_locals,
            &else_borrows,
            errors,
        );
    }

    /// M4 — a `while` loop is checked as "body executed once" merged
    /// against "body executed zero times" (the pre-loop state itself).
    /// This is a deliberate, documented scope cut: it verifies the body is
    /// internally consistent and that one pass through it agrees with
    /// skipping it entirely, but it does not compute a true fixed point
    /// across arbitrarily many iterations. See phase_specification.md, M4.
    fn check_while(&mut self, cond: &Expr, body: &phase_ast::Block, errors: &mut Vec<AnalysisError>) {
        self.check_expr(cond, errors);

        let base_locals = self.locals.clone();
        let base_borrows = self.borrows.clone();

        self.check_block(body, errors);
        let body_locals = std::mem::replace(&mut self.locals, base_locals.clone());
        let body_borrows = std::mem::replace(&mut self.borrows, base_borrows.clone());

        self.merge_branches(
            &base_locals,
            &base_borrows,
            &body_locals,
            &body_borrows,
            &base_locals,
            &base_borrows,
            errors,
        );
    }

    fn merge_branches(
        &mut self,
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
        let mut merged_locals = HashMap::new();
        for (name, base_var) in base_locals {
            let a = a_locals.get(name).expect("pre-existing local must survive both branches");
            let b = b_locals.get(name).expect("pre-existing local must survive both branches");

            let domain = if a.domain == b.domain {
                a.domain
            } else {
                errors.push(err(format!(
                    "entity '{name}' disagrees on domain across incoming branches (@{} vs @{})",
                    a.domain.map(|d| d.name()).unwrap_or("?"),
                    b.domain.map(|d| d.name()).unwrap_or("?"),
                )));
                None
            };

            let typestate = if a.typestate == b.typestate {
                a.typestate.clone()
            } else {
                errors.push(err(format!(
                    "entity '{name}' disagrees on typestate across incoming branches"
                )));
                None
            };

            let destroyed = if a.destroyed == b.destroyed {
                a.destroyed
            } else {
                errors.push(err(format!(
                    "entity '{name}' disagrees on destroyed-state across incoming branches (destroyed on one path but not the other)"
                )));
                true // conservative: treat as destroyed so further use is blocked
            };

            merged_locals.insert(
                name.clone(),
                VarState {
                    domain,
                    typestate,
                    destroyed,
                    borrow_of: base_var.borrow_of.clone(),
                    released: a.released || b.released,
                },
            );
        }
        self.locals = merged_locals;

        // Borrow targets that already existed before the branch must agree
        // on borrow state across both paths (e.g. released on one path but
        // not the other is a disagreement, same as any other property).
        let empty = BorrowState::default();
        let mut merged_borrows = HashMap::new();
        for (target, _) in base_borrows {
            let a_bs = a_borrows.get(target).unwrap_or(&empty);
            let b_bs = b_borrows.get(target).unwrap_or(&empty);
            let agrees =
                a_bs.readers == b_bs.readers && a_bs.writer == b_bs.writer;
            if !agrees {
                errors.push(err(format!(
                    "entity '{target}' disagrees on borrow state across incoming branches"
                )));
            }
            let chosen = if !a_bs.is_empty() { a_bs.clone() } else { b_bs.clone() };
            merged_borrows.insert(target.clone(), chosen);
        }
        self.borrows = merged_borrows;

        // A borrow first taken *inside* a branch (its target wasn't
        // borrowed before the branch) must be fully released before that
        // branch ends -- its handle variable is scoped to the branch and
        // has no way to be released afterward, so an outstanding borrow
        // here can never be released at all.
        let mut already_reported = HashSet::new();
        for (target, bs) in a_borrows.iter().chain(b_borrows.iter()) {
            if !base_borrows.contains_key(target) && !bs.is_empty() && already_reported.insert(target.clone()) {
                errors.push(err(format!(
                    "borrow of '{target}' taken inside a branch must be released before the branch ends"
                )));
            }
        }
    }

    fn check_var_decl(
        &mut self,
        name: &str,
        ty: Option<&TypeExpr>,
        domain: Option<Domain>,
        init: Option<&Expr>,
        errors: &mut Vec<AnalysisError>,
    ) {
        // `let v = borrow target mode;` — special-cased: `v` is not itself
        // a fresh domain entity, it's a handle onto `target`'s borrow state.
        if let Some(Expr::Borrow { target, mode }) = init {
            self.check_borrow_decl(name, target, *mode, errors);
            return;
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

        // Typestate: an explicit `Name<State>` annotation is validated and
        // resolved directly; otherwise, infer it from a call's declared
        // return typestate (e.g. `let p2 = decode(p1);`).
        let resolved_typestate = if let Some(t) = ty {
            resolve_stateful(t, self.state_machines, errors)
        } else if let Some(Expr::Call { callee, .. }) = init {
            self.fn_sigs
                .get(callee.as_str())
                .and_then(|sig| sig.return_state.clone())
        } else {
            None
        };

        self.locals.insert(
            name.to_string(),
            VarState {
                domain: resolved_domain,
                typestate: resolved_typestate,
                destroyed: false,
                borrow_of: None,
                released: false,
            },
        );

        if let Some(e) = init {
            self.check_expr(e, errors);
        }
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
        if target_var.destroyed {
            errors.push(err(format!(
                "entity '{target}' was destroyed; cannot borrow"
            )));
            return;
        }

        let bs = self.borrows.entry(target.to_string()).or_default();
        let ok = match mode {
            BorrowMode::Write => {
                if bs.writer.is_some() {
                    errors.push(err(format!(
                        "entity '{target}' already has an exclusive write borrow"
                    )));
                    false
                } else if !bs.readers.is_empty() {
                    errors.push(err(format!(
                        "entity '{target}' already has an active read borrow; cannot take exclusive write borrow"
                    )));
                    false
                } else {
                    bs.writer = Some(name.to_string());
                    true
                }
            }
            BorrowMode::Read => {
                if bs.writer.is_some() {
                    errors.push(err(format!(
                        "entity '{target}' already has an exclusive write borrow"
                    )));
                    false
                } else {
                    bs.readers.insert(name.to_string());
                    true
                }
            }
        };

        let (domain, typestate) = (target_var.domain, target_var.typestate.clone());
        if ok {
            self.locals.insert(
                name.to_string(),
                VarState {
                    domain,
                    typestate,
                    destroyed: false,
                    borrow_of: Some((target.to_string(), mode)),
                    released: false,
                },
            );
        } else {
            // The borrow itself was rejected, but `name` is still a
            // syntactically valid local going forward -- register it as an
            // inert placeholder (no active borrow) so later statements that
            // reference it (e.g. a subsequent `release name;`) get a
            // targeted, sensible error instead of a confusing cascade of
            // "unknown entity" complaints about a name the program did,
            // in fact, declare.
            self.locals.insert(
                name.to_string(),
                VarState {
                    domain,
                    typestate,
                    destroyed: false,
                    borrow_of: None,
                    released: false,
                },
            );
        }
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
                    "'{callee}' requires an @MMIO register reference (e.g. `REG_NAME @MMIO`) as its first argument"
                )));
            }
            None => unreachable!("arity already checked above"),
        }
    }

    fn check_expr(&mut self, expr: &Expr, errors: &mut Vec<AnalysisError>) {
        match expr {
            Expr::Call { callee, args } => self.check_call(callee, args, errors),
            Expr::FieldAccess { base, .. } => self.check_expr(base, errors),
            // Literals, bare idents, struct literals, domain refs, and
            // nested borrow-exprs (not via `let`) carry no further
            // domain/ownership/typestate obligations in M2/M3.
            _ => {}
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

        for (i, arg) in args.iter().enumerate() {
            match arg {
                Expr::Ident(name) => {
                    let Some(var) = self.locals.get(name) else {
                        errors.push(err(format!(
                            "unknown entity '{name}' passed to '{callee}'"
                        )));
                        continue;
                    };
                    if var.destroyed {
                        errors.push(err(format!(
                            "entity '{name}' was destroyed; cannot use as argument to '{callee}'"
                        )));
                        continue;
                    }

                    if let Some(Some(param_domain)) = sig.param_domains.get(i) {
                        if let Some(var_domain) = var.domain {
                            if var_domain != *param_domain {
                                errors.push(err(format!(
                                    "entity '{name}' is in domain @{} but '{callee}' expects @{} for this argument",
                                    var_domain.name(),
                                    param_domain.name()
                                )));
                            }
                        }
                    }

                    if let Some(Some((req_machine, req_state))) = sig.param_states.get(i) {
                        if let Some((have_machine, have_state)) = &var.typestate {
                            if have_machine != req_machine {
                                errors.push(err(format!(
                                    "entity '{name}' has typestate '{have_machine}' but '{callee}' expects typestate '{req_machine}'"
                                )));
                            } else if have_state != req_state {
                                errors.push(err(format!(
                                    "expected state {req_state}, found {have_state}"
                                )));
                            }
                        }
                    }
                }
                other => self.check_expr(other, errors),
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
        assert_single_error_containing(
            src,
            "no legal sync transition from @RAM",
        );
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
            fn main() {
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
            fn main() {
                buffer<u8, 16> x @RAM;
                if flag {
                    move x -> @DMA;
                } else {
                    move x -> @DEVICE;
                }
            }
        "#;
        // then: x is @DMA, else: x is @DEVICE -- they disagree at the join.
        assert_single_error_containing(src, "entity 'x' disagrees on domain across incoming branches");
    }

    #[test]
    fn if_without_else_must_agree_with_the_implicit_unchanged_path() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                if flag {
                    move x -> @DMA;
                }
            }
        "#;
        // No else means the implicit "else" is "x is still @RAM" -- which
        // disagrees with the then-branch's @DMA.
        assert_single_error_containing(src, "entity 'x' disagrees on domain across incoming branches");
    }

    #[test]
    fn destroy_on_only_one_branch_is_rejected() {
        let src = r#"
            fn main() {
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
            fn main() {
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
            fn main() {
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
            fn main() {
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
            fn main() {
                buffer<u8, 16> x @RAM;
                let v = borrow x read;
                if flag {
                    release v;
                } else {
                }
            }
        "#;
        assert_single_error_containing(src, "entity 'x' disagrees on borrow state across incoming branches");
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
        assert_single_error_containing(src, "entity 'x' disagrees on domain across incoming branches");
    }

    #[test]
    fn while_body_consistent_with_pre_loop_state_is_accepted() {
        let src = r#"
            fn main() {
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
            fn main() {
                buffer<u8, 16> x @RAM;
                while flag {
                    move x -> @DMA;
                }
            }
        "#;
        // Body-exit state (@DMA) disagrees with pre-loop state (@RAM) --
        // unsound if the loop runs a variable number of times.
        assert_single_error_containing(src, "entity 'x' disagrees on domain across incoming branches");
    }

    #[test]
    fn if_still_checks_statements_inside_each_branch_normally() {
        // The move inside the branch is illegal on its own terms (MMIO has
        // no legal move target to RAM), independent of any merge issue.
        let src = r#"
            fn main() {
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
            "'volatile_read' requires an @MMIO register reference",
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
}
