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

    let mut seen_fns = HashSet::new();
    for item in &program.items {
        let name = match item {
            Item::Fn(f) => &f.name,
            Item::ExternFn(f) => &f.name,
            _ => continue,
        };
        if !seen_fns.insert(name.as_str()) {
            errors.push(err(format!("function '{name}' is declared more than once")));
        }
    }

    for item in &program.items {
        if let Item::Fn(f) = item {
            let mut checker = FnChecker::new(
                &f.name,
                &entity_domains,
                &fn_sigs,
                &state_machines,
                &initial_states,
            );
            checker.check_fn(f, &mut errors);
        }
    }

    // One mistake can trip the same rule more than once (e.g. the same
    // entity passed twice in one call); report each distinct error once.
    let mut seen = HashSet::new();
    errors.retain(|e| seen.insert(e.message.clone()));

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
    /// Typestate values are linear: passing one to a parameter that
    /// requires a state (or returning it) consumes it, and the old handle
    /// can never be used again. `Some(how)` once that has happened, where
    /// `how` is a human-readable description for diagnostics.
    consumed: Option<String>,
    /// `Some(d)` for a function parameter: the domain the caller handed it
    /// over in, which it must be back in when the function returns.
    param_entry_domain: Option<Option<Domain>>,
    /// A `let v = borrow ..` whose borrow was rejected (already reported).
    /// Kept as a name so later lines get no "unknown entity" noise, and its
    /// `release` is accepted silently.
    rejected_borrow: bool,
}

impl VarState {
    fn local(domain: Option<Domain>, typestate: Option<Typestate>) -> Self {
        VarState {
            domain,
            typestate,
            destroyed: false,
            borrow_of: None,
            released: false,
            consumed: None,
            param_entry_domain: None,
            rejected_borrow: false,
        }
    }

    /// Whether this local carries any physical/protocol obligations at all
    /// (as opposed to a plain scalar).
    fn is_tracked(&self) -> bool {
        self.domain.is_some() || self.typestate.is_some() || self.borrow_of.is_some()
    }

    fn is_param(&self) -> bool {
        self.param_entry_domain.is_some()
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
    fn_sigs: &'a HashMap<&'a str, FnSig>,
    state_machines: &'a HashMap<&'a str, HashSet<&'a str>>,
    initial_states: &'a HashMap<&'a str, &'a str>,
    entity_domains: &'a HashMap<&'a str, Option<Domain>>,
    locals: HashMap<String, VarState>,
    /// keyed by the *target* entity name being borrowed from
    borrows: HashMap<String, BorrowState>,
    /// Set once every path through the statements checked so far has hit a
    /// `return`. Code after that point is unreachable, and the state at that
    /// point must not be merged into the fall-through state of a branch.
    diverged: bool,
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
        entity_domains: &'a HashMap<&'a str, Option<Domain>>,
        fn_sigs: &'a HashMap<&'a str, FnSig>,
        state_machines: &'a HashMap<&'a str, HashSet<&'a str>>,
        initial_states: &'a HashMap<&'a str, &'a str>,
    ) -> Self {
        FnChecker {
            fn_name: fn_name.to_string(),
            entity_domains,
            fn_sigs,
            state_machines,
            initial_states,
            locals: HashMap::new(),
            borrows: HashMap::new(),
            diverged: false,
        }
    }

    fn check_fn(&mut self, f: &FnDecl, errors: &mut Vec<AnalysisError>) {
        // Parameters are locals too: register them up front (using the
        // domain/typestate already resolved once for this function's
        // signature) so a body that uses a parameter directly -- not just
        // ones re-bound through a `let` -- is checked correctly.
        if let Some(sig) = self.fn_sigs.get(f.name.as_str()) {
            for (i, p) in f.params.iter().enumerate() {
                if self.locals.contains_key(&p.name) {
                    errors.push(err(format!(
                        "parameter '{}' is declared more than once in fn '{}'",
                        p.name, f.name
                    )));
                    continue;
                }
                let domain = sig.param_domains.get(i).copied().flatten();
                let mut var = VarState::local(domain, sig.param_states.get(i).cloned().flatten());
                var.param_entry_domain = Some(domain);
                self.locals.insert(p.name.clone(), var);
            }
        }

        self.check_block(&f.body, errors);

        // Falling off the end of the body is an implicit `return;`.
        if !self.diverged {
            self.check_exit_obligations(errors);
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
                    errors.push(err(format!(
                        "borrow '{name}' of '{target}' is still live when '{fn_name}' returns; release it first"
                    )));
                }
                continue;
            }
            if var.destroyed || var.consumed.is_some() {
                continue;
            }
            match (var.param_entry_domain, var.domain) {
                (Some(Some(entry)), Some(now)) if entry != now => {
                    errors.push(err(format!(
                        "parameter '{name}' must be back in @{} when '{fn_name}' returns, but it is in @{}",
                        entry.name(),
                        now.name()
                    )));
                }
                (Some(_), _) => {}
                (None, Some(now)) if is_device_owned(now) => {
                    errors.push(err(format!(
                        "entity '{name}' is still owned by @{} when '{fn_name}' returns; sync({name}) before it goes out of scope",
                        now.name()
                    )));
                }
                (None, _) => {}
            }
        }
    }

    fn check_return_value(&mut self, name: &str, errors: &mut Vec<AnalysisError>) {
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
            errors.push(err(format!(
                "entity '{name}' was destroyed; cannot return it"
            )));
            return;
        }
        if let Some(how) = &var.consumed {
            errors.push(err(format!(
                "value '{name}' was already consumed ({how}); cannot return it"
            )));
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
                    errors.push(err(format!(
                        "expected state {req_state}, found {have_state}"
                    )));
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
            self.locals.get_mut(name).unwrap().consumed =
                Some(format!("returned from '{fn_name}'"));
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
    fn owned_target(
        &self,
        name: &str,
        verb: &str,
        errors: &mut Vec<AnalysisError>,
    ) -> Option<VarState> {
        let Some(var) = self.locals.get(name) else {
            errors.push(err(format!("unknown entity '{name}'")));
            return None;
        };
        if let Some((target, _)) = &var.borrow_of {
            errors.push(err(format!(
                "'{name}' is a borrow of '{target}'; cannot {verb} a borrow handle (release it, then {verb} '{target}')"
            )));
            return None;
        }
        if var.destroyed {
            let msg = if verb == "destroy" {
                format!("entity '{name}' was already destroyed")
            } else {
                format!("entity '{name}' was destroyed; cannot {verb}")
            };
            errors.push(err(msg));
            return None;
        }
        if let Some(how) = &var.consumed {
            errors.push(err(format!(
                "value '{name}' was already consumed ({how}); cannot {verb}"
            )));
            return None;
        }
        if self.has_live_borrow(name) {
            errors.push(err(format!(
                "entity '{name}' has a live borrow; cannot {verb}"
            )));
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
                self.locals.get_mut(name).unwrap().domain = Some(*to);
            }

            Stmt::Sync { name } => {
                let Some(var) = self.owned_target(name, "sync", errors) else {
                    return;
                };
                let Some(from) = var.domain else { return };
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
                if var.rejected_borrow {
                    return; // the borrow itself was already reported
                }
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
                let Some(var) = self.owned_target(name, "destroy", errors) else {
                    return;
                };
                if var.is_param() {
                    errors.push(err(format!(
                        "cannot destroy parameter '{name}': the caller still owns it"
                    )));
                    return;
                }
                if let Some(d) = var.domain.filter(|d| is_device_owned(*d)) {
                    errors.push(err(format!(
                        "entity '{name}' is still owned by @{}; sync({name}) before destroying it",
                        d.name()
                    )));
                    return;
                }
                self.locals.get_mut(name).unwrap().destroyed = true;
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
                errors.push(err(format!(
                    "unreachable statement after `return` in fn '{}'",
                    self.fn_name
                )));
                return;
            }
            self.check_stmt(stmt, errors);
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
            self.check_block(b, errors);
        }
        let out_locals = std::mem::replace(&mut self.locals, saved_locals);
        let out_borrows = std::mem::replace(&mut self.borrows, saved_borrows);
        let out_diverged = std::mem::replace(&mut self.diverged, saved_diverged);
        (out_locals, out_borrows, out_diverged)
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

            let domain = if a.domain == b.domain {
                a.domain
            } else {
                errors.push(err(format!(
                    "entity '{name}' disagrees on domain across incoming branches (@{} vs @{}){loop_note}",
                    a.domain.map(|d| d.name()).unwrap_or("?"),
                    b.domain.map(|d| d.name()).unwrap_or("?"),
                )));
                None
            };

            let typestate = if a.typestate == b.typestate {
                a.typestate.clone()
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

            merged_locals.insert(
                name.clone(),
                VarState {
                    domain,
                    typestate,
                    destroyed,
                    borrow_of: base_var.borrow_of.clone(),
                    released: a.released || b.released,
                    consumed,
                    param_entry_domain: base_var.param_entry_domain,
                    rejected_borrow: base_var.rejected_borrow,
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
        if self.locals.contains_key(name) {
            errors.push(err(format!(
                "'{name}' is already declared in fn '{}'; PHASE does not allow shadowing or redeclaration",
                self.fn_name
            )));
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
                    errors.push(err(format!(
                        "cannot bind '{name}' to entity '{src}': entities are not copyable (use `borrow {src} read|write` for a second name)"
                    )));
                    return;
                }
            }
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
                    errors.push(err(format!(
                        "'{name}' is declared as {} but '{callee}' returns {}",
                        fmt_state(&decl),
                        fmt_state(&actual)
                    )));
                }
                Some(actual)
            }
            (Some(decl), Some(None)) => {
                let callee = match init {
                    Some(Expr::Call { callee, .. }) => callee.as_str(),
                    _ => unreachable!(),
                };
                errors.push(err(format!(
                    "'{name}' is declared as {} but '{callee}' does not return a typestate value; a state can only come from a function that returns it",
                    fmt_state(&decl)
                )));
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
                    errors.push(err(format!(
                        "'{name}' cannot be created directly in state {}: a new value starts in {}<{}>, and later states can only come from a function that returns them",
                        fmt_state(&decl),
                        decl.0,
                        initial.unwrap_or("?")
                    )));
                    None
                }
            }
            (None, None) => None,
        };

        self.locals.insert(
            name.to_string(),
            VarState::local(resolved_domain, resolved_typestate),
        );
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
        let rejection = if target_var.destroyed {
            Some(format!("entity '{target}' was destroyed; cannot borrow"))
        } else if let Some(how) = &target_var.consumed {
            Some(format!(
                "value '{target}' was already consumed ({how}); cannot borrow"
            ))
        } else if let Some((owner, _)) = &target_var.borrow_of {
            Some(format!(
                "'{target}' is itself a borrow of '{owner}'; borrow '{owner}' directly instead"
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
            errors.push(err(msg));
            // Register an inert placeholder so later references to `name`
            // get targeted errors instead of "unknown entity" noise.
            let mut placeholder = VarState::local(None, None);
            placeholder.rejected_borrow = true;
            self.locals.insert(name.to_string(), placeholder);
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
        let mut var = VarState::local(domain, typestate);
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

    /// A bare name used as a *value* (a condition, an assignment right-hand
    /// side, a `volatile_write` value): it must exist, and it must be a
    /// plain scalar -- an entity has no scalar value to read.
    fn check_value_ident(&self, name: &str, errors: &mut Vec<AnalysisError>) {
        match self.locals.get(name) {
            None => errors.push(err(format!("unknown name '{name}'"))),
            Some(var) if var.is_tracked() => errors.push(err(format!(
                "'{name}' is an entity, not a scalar value; it cannot be used in an expression"
            ))),
            Some(var) if var.destroyed => {
                errors.push(err(format!("'{name}' was destroyed; cannot use it")))
            }
            Some(_) => {}
        }
    }

    /// Shared access check for using an entity's *contents* directly
    /// (call argument, field read): it must be live, owned by the CPU, and
    /// not currently lent out through a borrow.
    fn check_entity_access(
        &self,
        name: &str,
        usage: &str,
        errors: &mut Vec<AnalysisError>,
    ) -> bool {
        let Some(var) = self.locals.get(name) else {
            return false;
        };
        if var.destroyed {
            errors.push(err(format!(
                "entity '{name}' was destroyed; cannot {usage}"
            )));
            return false;
        }
        if let Some(how) = &var.consumed {
            errors.push(err(format!(
                "value '{name}' was already consumed ({how}); cannot {usage}"
            )));
            return false;
        }
        if let Some((target, _)) = &var.borrow_of {
            if var.released {
                errors.push(err(format!(
                    "borrow '{name}' of '{target}' was already released; cannot {usage}"
                )));
                return false;
            }
        } else if let Some(bs) = self.borrows.get(name).filter(|b| !b.is_empty()) {
            errors.push(err(format!(
                "entity '{name}' is borrowed by '{}'; it cannot be used directly until that borrow is released",
                bs.any_holder().unwrap_or_default()
            )));
            return false;
        }
        true
    }

    fn check_expr(&mut self, expr: &Expr, errors: &mut Vec<AnalysisError>) {
        match expr {
            Expr::Call { callee, args } => self.check_call(callee, args, errors),
            Expr::Ident(name) => self.check_value_ident(name, errors),
            Expr::FieldAccess { base, .. } => match base.as_ref() {
                Expr::Ident(name) => match self.locals.get(name) {
                    None => errors.push(err(format!("unknown name '{name}'"))),
                    Some(var) => {
                        let domain = var.domain;
                        if self.check_entity_access(name, "read its fields", errors) {
                            if let Some(d) = domain.filter(|d| is_device_owned(*d)) {
                                errors.push(err(format!(
                                    "entity '{name}' is owned by @{}; sync({name}) before reading its fields",
                                    d.name()
                                )));
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
            Expr::Binary { lhs, rhs, .. } => {
                self.check_expr(lhs, errors);
                self.check_expr(rhs, errors);
            }
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
        // occurrence is a read-only borrow -- otherwise the callee would
        // see two names for memory at least one of them may write.
        let mut roots: Vec<(String, bool)> = Vec::new(); // (root, read_only)
        let mut to_consume: Vec<String> = Vec::new();

        for (i, arg) in args.iter().enumerate() {
            match arg {
                Expr::Ident(name) => {
                    let Some(var) = self.locals.get(name).cloned() else {
                        errors.push(err(format!("unknown entity '{name}' passed to '{callee}'")));
                        continue;
                    };
                    if !self.check_entity_access(
                        name,
                        &format!("use as argument to '{callee}'"),
                        errors,
                    ) {
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

                    if let Some(Some(required)) = sig.param_states.get(i) {
                        let (req_machine, req_state) = required;
                        match &var.typestate {
                            Some((have_machine, have_state)) => {
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
                            None => errors.push(err(format!(
                                "'{callee}' requires {} but '{name}' has no proven typestate",
                                fmt_state(required)
                            ))),
                        }
                        // Linear: handing a value to a state-requiring
                        // parameter consumes it. A borrow handle cannot be
                        // consumed (it doesn't own the value).
                        if var.borrow_of.is_some() {
                            errors.push(err(format!(
                                "'{callee}' takes ownership of a {} value, but '{name}' is only a borrow",
                                fmt_state(required)
                            )));
                        } else {
                            to_consume.push(name.clone());
                        }
                    }

                    if var.is_tracked() {
                        let (root, read_only) = match &var.borrow_of {
                            Some((target, mode)) => (target.clone(), *mode == BorrowMode::Read),
                            None => (name.clone(), false),
                        };
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
                errors.push(err(format!(
                    "entity '{root}' is passed to '{callee}' more than once; aliased arguments are only allowed through read borrows"
                )));
            }
        }

        for name in to_consume {
            if let Some(v) = self.locals.get_mut(&name) {
                v.consumed = Some(format!("passed to '{callee}'"));
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
            "entity 'x' disagrees on domain across incoming branches",
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
        assert_single_error_containing(src, "'volatile_read' requires an @MMIO register reference");
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
            "fn main() { buffer<u8, 16> x @RAM; let a = borrow x read; let b = borrow x read; fir(a, b); release a; release b; }",
        ));
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
