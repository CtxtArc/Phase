//! PHASE M2: the domain/ownership/borrow analyzer.
//!
//! Scope (matches the roadmap in phase_specification.md, M2): straight-line
//! function bodies only — the AST has no `if`/`while` yet, so control flow
//! and `ENTITY_PHI` merging (M4) simply don't arise here. This pass is
//! responsible for bug-gallery items #1-#4:
//!
//!   1. use of a `@DMA`/`@DEVICE`-domain entity where a different domain
//!      was required, without an intervening `sync`
//!   2. `move`/`sync`/`destroy` while a borrow is live
//!   3. conflicting borrows (two writers, or a writer alongside readers)
//!   4. use of a destroyed entity
//!
//! plus the domain-transition-table check from spec §2.4 (illegal `move`/
//! `sync` pairs).

use phase_ast::{BorrowMode, Domain, Expr, FnDecl, Item, Program, Stmt};
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

/// Analyze a whole program. Returns `Ok(())` if every function body is
/// physically consistent, otherwise every violation found (the checker
/// does not stop at the first error — it keeps going, treating the
/// erroring statement as a no-op, so one mistake doesn't cascade into a
/// wall of unrelated-looking follow-on errors).
pub fn analyze(program: &Program) -> Result<(), Vec<AnalysisError>> {
    let entity_domains: HashMap<&str, Option<Domain>> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Entity(e) => Some((e.name.as_str(), e.domain)),
            _ => None,
        })
        .collect();

    // name -> per-parameter declared domain (None = unconstrained)
    let mut fn_sigs: HashMap<&str, Vec<Option<Domain>>> = HashMap::new();
    for item in &program.items {
        match item {
            Item::Fn(f) => {
                fn_sigs.insert(f.name.as_str(), f.params.iter().map(|p| p.domain).collect());
            }
            Item::ExternFn(f) => {
                fn_sigs.insert(f.name.as_str(), f.params.iter().map(|p| p.domain).collect());
            }
            _ => {}
        }
    }

    let mut errors = Vec::new();
    for item in &program.items {
        if let Item::Fn(f) = item {
            let mut checker = FnChecker::new(&entity_domains, &fn_sigs);
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
    fn_sigs: &'a HashMap<&'a str, Vec<Option<Domain>>>,
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
        fn_sigs: &'a HashMap<&'a str, Vec<Option<Domain>>>,
    ) -> Self {
        FnChecker {
            entity_domains,
            fn_sigs,
            locals: HashMap::new(),
            borrows: HashMap::new(),
        }
    }

    fn check_fn(&mut self, f: &FnDecl, errors: &mut Vec<AnalysisError>) {
        for stmt in &f.body.stmts {
            self.check_stmt(stmt, errors);
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
        }
    }

    fn check_var_decl(
        &mut self,
        name: &str,
        ty: Option<&phase_ast::TypeExpr>,
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
            (None, Some(phase_ast::TypeExpr::Named(n))) => {
                self.entity_domains.get(n.as_str()).copied().flatten()
            }
            (None, Some(phase_ast::TypeExpr::Buffer { .. })) => {
                errors.push(err(format!(
                    "entity '{name}' has no domain: buffer types require an explicit @domain"
                )));
                None
            }
            (None, None) => None,
        };

        self.locals.insert(
            name.to_string(),
            VarState {
                domain: resolved_domain,
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

        if ok {
            self.locals.insert(
                name.to_string(),
                VarState {
                    domain: target_var.domain,
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
                    domain: target_var.domain,
                    destroyed: false,
                    borrow_of: None,
                    released: false,
                },
            );
        }
    }

    fn check_expr(&mut self, expr: &Expr, errors: &mut Vec<AnalysisError>) {
        match expr {
            Expr::Call { callee, args } => self.check_call(callee, args, errors),
            Expr::FieldAccess { base, .. } => self.check_expr(base, errors),
            // Literals, bare idents, struct literals, domain refs, and
            // nested borrow-exprs (not via `let`) carry no further
            // domain/ownership obligations in M2.
            _ => {}
        }
    }

    fn check_call(&mut self, callee: &str, args: &[Expr], errors: &mut Vec<AnalysisError>) {
        // `volatile_read`/`volatile_write` are builtins, not user/extern
        // fns: their MMIO-domain argument is checked syntactically by the
        // parser (it must be a `DomainRef`), so there's nothing more to
        // verify here in M2.
        if callee == "volatile_read" || callee == "volatile_write" {
            return;
        }

        let Some(params) = self.fn_sigs.get(callee) else {
            errors.push(err(format!("call to unknown function '{callee}'")));
            return;
        };
        let params = params.clone();

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
                    if let Some(Some(param_domain)) = params.get(i) {
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
}
