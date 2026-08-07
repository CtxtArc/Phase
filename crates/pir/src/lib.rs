//! PHASE M5: PIR — the Phase Intermediate Representation (spec §7).
//!
//! PIR sits between the (already-analyzed) AST and C codegen. It flattens
//! each function into an ordered list of instructions and resolves every
//! type down to something codegen can turn directly into C.
//!
//! M5 targeted the straight-line subset only; M8 adds real `if`/`while`
//! lowering (`PirInst::If`/`PirInst::While`), so a body containing control
//! flow now compiles instead of being rejected. `UnsupportedControlFlow`
//! is kept as a variant (unused by this crate today) rather than deleted,
//! since removing an error variant is itself a breaking API change for
//! anything matching on `PirError` -- see also `phase_specification.md`
//! §9, Milestone M8.
//!
//! Condition expressions in `if`/`while` lower to `PirExpr`, a small
//! subset of `phase_ast::Expr` (identifiers, literals, and binary
//! comparisons/arithmetic) -- enough for every bug-gallery and demo
//! program. A condition using anything richer is a clear `UnsupportedExpr`
//! error, not a silent guess.
//!
//! M9 adds `PirInst::Assign` (`name = expr;`), lowering the same way --
//! see `phase_specification.md` §9, Milestone M9. This is the real
//! prerequisite for a `while` loop that terminates on its own instead of
//! running zero or infinite times, since M8 alone gave conditions nothing
//! that could change between iterations.
//!
//! M11 extends `PirExpr` with `VolatileRead` so `volatile_read(REG @MMIO)`
//! can appear directly inside a condition or assignment RHS -- e.g.
//! `while volatile_read(STATUS @MMIO) == 0 { .. }`, a real hardware
//! polling loop expressed in PHASE source rather than requiring
//! hand-written C. See `phase_specification.md` §9, Milestone M11.
//!
//! PIR generation trusts that the program has already passed
//! `phase_analysis::analyze` — it does not re-derive domain/borrow/
//! typestate safety, only enough domain bookkeeping (which domain an
//! entity is in right now) to pick the correct runtime call for `sync`.

use phase_ast::{BinOp, BorrowMode, Domain, Expr, Item, Program, Stmt, TypeExpr};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum PirError {
    UnsupportedControlFlow { context: String },
    UnknownType { name: String },
    MissingType { context: String },
    UnsupportedExpr { context: String },
}

impl std::fmt::Display for PirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PirError::UnsupportedControlFlow { context } => write!(
                f,
                "codegen does not yet support control flow (M5 scope; see roadmap M6+): {context}"
            ),
            PirError::UnknownType { name } => {
                write!(f, "unknown type '{name}': no entity declaration and no builtin scalar mapping")
            }
            PirError::MissingType { context } => write!(
                f,
                "cannot generate code for {context}: no declared type (type inference is not yet supported by codegen)"
            ),
            PirError::UnsupportedExpr { context } => {
                write!(f, "codegen does not yet support this expression form: {context}")
            }
        }
    }
}

/// A type, fully resolved to something codegen can lower directly.
/// Typestate is erased here — it's a compile-time-only proof and carries
/// no runtime representation (the same way Rust erases generics).
#[derive(Debug, Clone, PartialEq)]
pub enum PirType {
    /// An already-resolved C scalar type name, e.g. `"float"`, `"uint8_t"`.
    Scalar(String),
    /// The name of a struct generated from a PHASE `entity` declaration.
    Named(String),
    Buffer { elem: Box<PirType>, len: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PirLiteral {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

/// A condition expression, as it appears in `if`/`while`. Deliberately a
/// small subset of `phase_ast::Expr` -- just enough to lower the
/// comparisons and boolean locals the M4 branch-merge analyzer already
/// proved safe. Anything richer is a clear `UnsupportedExpr`, not a guess.
#[derive(Debug, Clone, PartialEq)]
pub enum PirExpr {
    Ident(String),
    IntLit(i64),
    FloatLit(f64),
    BoolLit(bool),
    Binary {
        op: BinOp,
        lhs: Box<PirExpr>,
        rhs: Box<PirExpr>,
    },
    /// `volatile_read(REG @MMIO)` used directly inside a condition or
    /// assignment right-hand side (M11) -- e.g.
    /// `while volatile_read(STATUS @MMIO) == 0 { .. }`, a real hardware
    /// polling loop expressed in PHASE source instead of requiring
    /// hand-written C. Deliberately restricted to this one builtin:
    /// general `extern fn` calls inside a condition would be re-invoked
    /// every loop iteration with much less clear ordering/purity
    /// guarantees than a register read, which is a separate design
    /// question this milestone doesn't attempt to answer.
    VolatileRead { register: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallArg {
    Entity(String),
    Literal(PirLiteral),
    /// `REG_NAME @MMIO` — the register argument to `volatile_read`/
    /// `volatile_write` (M6). Distinct from `Entity` because it doesn't
    /// refer to a declared local; it names a simulated memory-mapped
    /// register that codegen materializes as a `volatile` global.
    MmioRegister(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum PirInst {
    /// A fresh, uninitialized (or zero-initialized) local of a given type.
    EntityCreate {
        name: String,
        ty: PirType,
        domain: Option<Domain>,
    },
    /// `let s: Sample @RAM = Sample { value: 0.0 };`
    EntityCreateWithFields {
        name: String,
        ty: PirType,
        domain: Option<Domain>,
        fields: Vec<(String, PirLiteral)>,
    },
    /// `let n: i32 = 0;` — a fresh scalar local initialized from a bare
    /// literal. Split out from `EntityCreateWithFields` (struct literals)
    /// because it declares with `= <value>;`, not a designated
    /// initializer -- added in M8 so `while` loop counters/flags have
    /// somewhere to come from.
    EntityCreateWithLiteral {
        name: String,
        ty: PirType,
        domain: Option<Domain>,
        value: PirLiteral,
    },
    /// `let p2 = decode(p1);` — a fresh local initialized from a call's
    /// return value.
    CallAssign {
        name: String,
        ty: PirType,
        domain: Option<Domain>,
        callee: String,
        args: Vec<CallArg>,
    },
    /// `move name -> to;` — compile-time-only in the v0.1 simulation model
    /// (see spec §6.1/§7.4): everything lives in one address space, so
    /// there is nothing to physically move. Kept in PIR so codegen can at
    /// least emit a explanatory comment at the call site.
    EntityMove { name: String, to: Domain },
    /// `sync(name);` — `from` is the domain the entity was in going into
    /// the sync (always `Dma` or `Device`, enforced by the M2 analyzer),
    /// which determines which simulated wait function to call.
    EntitySync { name: String, from: Domain },
    EntityBorrow {
        name: String,
        target: String,
        mode: BorrowMode,
    },
    EntityRelease { name: String },
    EntityDestroy { name: String },
    Call { callee: String, args: Vec<CallArg> },
    Return { value: Option<String> },
    /// `if cond { .. } else { .. }` (M8). `else_body` is empty, not
    /// absent, when there's no `else` clause -- codegen just emits a bare
    /// `if` block in that case.
    If {
        cond: PirExpr,
        then_body: Vec<PirInst>,
        else_body: Vec<PirInst>,
    },
    /// `while cond { .. }` (M8). Lowers to a real C `while`; see spec §10 /
    /// the M4 analyzer docs for why this is sound without a fixed-point
    /// loop analysis (bodies are checked as "ran zero or one times").
    While { cond: PirExpr, body: Vec<PirInst> },
    /// `name = expr;` (M9). The real prerequisite for a `while` loop that
    /// can terminate on its own -- e.g. a counter incremented each pass.
    Assign { name: String, value: PirExpr },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PirEntityDef {
    pub name: String,
    pub fields: Vec<(String, PirType)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PirFnSig {
    pub name: String,
    pub params: Vec<(String, PirType)>,
    pub return_type: Option<PirType>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PirFn {
    pub sig: PirFnSig,
    pub body: Vec<PirInst>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct PirProgram {
    pub entities: Vec<PirEntityDef>,
    pub extern_fns: Vec<PirFnSig>,
    pub fns: Vec<PirFn>,
    /// Every distinct `@MMIO` register name referenced anywhere in the
    /// program via `volatile_read`/`volatile_write` (M6), in first-seen
    /// order. Codegen materializes each of these as a `volatile` global.
    pub mmio_registers: Vec<String>,
}

/// Map a PHASE scalar type name to its C representation. Returns `None`
/// for names that aren't builtin scalars (they're assumed to be
/// `entity`-declared struct types instead).
fn builtin_scalar_c_type(name: &str) -> Option<&'static str> {
    Some(match name {
        "u8" => "uint8_t",
        "u16" => "uint16_t",
        "u32" => "uint32_t",
        "u64" => "uint64_t",
        "i8" => "int8_t",
        "i16" => "int16_t",
        "i32" => "int32_t",
        "i64" => "int64_t",
        "f32" => "float",
        "f64" => "double",
        "bool" => "bool",
        _ => return None,
    })
}

fn resolve_type(ty: &TypeExpr, known_entities: &HashMap<&str, ()>) -> Result<PirType, PirError> {
    match ty {
        TypeExpr::Named(n) => {
            if let Some(c) = builtin_scalar_c_type(n) {
                Ok(PirType::Scalar(c.to_string()))
            } else if known_entities.contains_key(n.as_str()) {
                Ok(PirType::Named(n.clone()))
            } else {
                Err(PirError::UnknownType { name: n.clone() })
            }
        }
        // Typestate is erased at the PIR level -- resolve by base name only.
        TypeExpr::Stateful { name, .. } => resolve_type(&TypeExpr::Named(name.clone()), known_entities),
        TypeExpr::Buffer { elem, len } => Ok(PirType::Buffer {
            elem: Box::new(resolve_type(elem, known_entities)?),
            len: *len,
        }),
    }
}

/// Lower an already-analyzed `Program` to PIR.
pub fn build(program: &Program) -> Result<PirProgram, PirError> {
    let known_entities: HashMap<&str, ()> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Entity(e) => Some((e.name.as_str(), ())),
            _ => None,
        })
        .collect();

    let mut out = PirProgram::default();

    for item in &program.items {
        match item {
            Item::Entity(e) => {
                let mut fields = Vec::new();
                for f in &e.fields {
                    fields.push((f.name.clone(), resolve_type(&f.ty, &known_entities)?));
                }
                out.entities.push(PirEntityDef {
                    name: e.name.clone(),
                    fields,
                });
            }
            Item::ExternFn(f) => {
                out.extern_fns.push(lower_fn_sig(
                    &f.name,
                    &f.params,
                    &f.return_type,
                    &known_entities,
                )?);
            }
            Item::Fn(f) => {
                let sig = lower_fn_sig(&f.name, &f.params, &f.return_type, &known_entities)?;
                let mut lowerer = Lowerer {
                    known_entities: &known_entities,
                    domains: HashMap::new(),
                };
                for p in &f.params {
                    if let Some(d) = p.domain {
                        lowerer.domains.insert(p.name.clone(), d);
                    }
                }
                let body = lowerer.lower_block(&f.body, &f.name)?;
                out.fns.push(PirFn { sig, body });
            }
            Item::State(_) => {} // typestate is compile-time-only; nothing to lower
        }
    }

    out.mmio_registers = collect_mmio_registers(&out.fns);

    Ok(out)
}

/// Scan every function body -- recursively through `if`/`while` bodies,
/// and through condition/assignment `PirExpr`s (M11's `VolatileRead`) --
/// for every distinct MMIO register name, in first-seen order with
/// duplicates removed. Codegen needs one `volatile` global per distinct
/// register name, not one per use site.
///
/// Recursing into `If`/`While` was a latent gap from M8: nothing
/// previously exercised a `volatile_read`/`volatile_write` inside a
/// branch or loop body, so a top-level-only scan happened to be enough
/// until M11 needed `VolatileRead` in a `while` condition to surface it.
fn collect_mmio_registers(fns: &[PirFn]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();

    fn note_args(args: &[CallArg], seen: &mut std::collections::HashSet<String>, out: &mut Vec<String>) {
        for a in args {
            if let CallArg::MmioRegister(name) = a {
                if seen.insert(name.clone()) {
                    out.push(name.clone());
                }
            }
        }
    }

    fn note_expr(e: &PirExpr, seen: &mut std::collections::HashSet<String>, out: &mut Vec<String>) {
        match e {
            PirExpr::VolatileRead { register } => {
                if seen.insert(register.clone()) {
                    out.push(register.clone());
                }
            }
            PirExpr::Binary { lhs, rhs, .. } => {
                note_expr(lhs, seen, out);
                note_expr(rhs, seen, out);
            }
            PirExpr::Ident(_) | PirExpr::IntLit(_) | PirExpr::FloatLit(_) | PirExpr::BoolLit(_) => {}
        }
    }

    fn walk(body: &[PirInst], seen: &mut std::collections::HashSet<String>, out: &mut Vec<String>) {
        for inst in body {
            match inst {
                PirInst::CallAssign { args, .. } | PirInst::Call { args, .. } => {
                    note_args(args, seen, out)
                }
                PirInst::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    note_expr(cond, seen, out);
                    walk(then_body, seen, out);
                    walk(else_body, seen, out);
                }
                PirInst::While { cond, body } => {
                    note_expr(cond, seen, out);
                    walk(body, seen, out);
                }
                PirInst::Assign { value, .. } => note_expr(value, seen, out),
                _ => {}
            }
        }
    }

    for f in fns {
        walk(&f.body, &mut seen, &mut out);
    }
    out
}

fn lower_fn_sig(
    name: &str,
    params: &[phase_ast::Param],
    return_type: &Option<TypeExpr>,
    known_entities: &HashMap<&str, ()>,
) -> Result<PirFnSig, PirError> {
    let mut lowered_params = Vec::new();
    for p in params {
        lowered_params.push((p.name.clone(), resolve_type(&p.ty, known_entities)?));
    }
    let lowered_return = return_type
        .as_ref()
        .map(|t| resolve_type(t, known_entities))
        .transpose()?;
    Ok(PirFnSig {
        name: name.to_string(),
        params: lowered_params,
        return_type: lowered_return,
    })
}

struct Lowerer<'a> {
    known_entities: &'a HashMap<&'a str, ()>,
    /// Tracks each local's *current* domain, just enough to know whether a
    /// `sync` is waiting on a DMA or a device transfer. Safety itself was
    /// already proven by `phase_analysis`; this is bookkeeping, not a check.
    domains: HashMap<String, Domain>,
}

impl<'a> Lowerer<'a> {
    fn lower_block(&mut self, block: &phase_ast::Block, fn_name: &str) -> Result<Vec<PirInst>, PirError> {
        let mut out = Vec::new();
        for stmt in &block.stmts {
            self.lower_stmt(stmt, fn_name, &mut out)?;
        }
        Ok(out)
    }

    fn resolve(&self, ty: &TypeExpr) -> Result<PirType, PirError> {
        resolve_type(ty, self.known_entities)
    }

    fn lower_stmt(
        &mut self,
        stmt: &Stmt,
        fn_name: &str,
        out: &mut Vec<PirInst>,
    ) -> Result<(), PirError> {
        match stmt {
            Stmt::VarDecl {
                name,
                ty,
                domain,
                init,
            } => self.lower_var_decl(name, ty.as_ref(), *domain, init.as_ref(), out),

            Stmt::Move { name, to } => {
                self.domains.insert(name.clone(), *to);
                out.push(PirInst::EntityMove {
                    name: name.clone(),
                    to: *to,
                });
                Ok(())
            }

            Stmt::Sync { name } => {
                let from = self.domains.get(name).copied().unwrap_or(Domain::Ram);
                self.domains.insert(name.clone(), Domain::Ram);
                out.push(PirInst::EntitySync {
                    name: name.clone(),
                    from,
                });
                Ok(())
            }

            Stmt::Release { name } => {
                out.push(PirInst::EntityRelease { name: name.clone() });
                Ok(())
            }

            Stmt::Destroy { name } => {
                out.push(PirInst::EntityDestroy { name: name.clone() });
                Ok(())
            }

            Stmt::Expr(e) => self.lower_expr_stmt(e, out),

            Stmt::Return(value) => {
                let name = match value {
                    None => None,
                    Some(Expr::Ident(n)) => Some(n.clone()),
                    Some(_) => {
                        return Err(PirError::UnsupportedExpr {
                            context: format!("return expression in fn '{fn_name}' (only a bare identifier or no value is supported)"),
                        })
                    }
                };
                out.push(PirInst::Return { value: name });
                Ok(())
            }

            Stmt::If {
                cond,
                then_block,
                else_block,
            } => {
                let context = format!("`if` inside fn '{fn_name}'");
                let pir_cond = lower_pir_expr(cond, &context)?;

                // Domain bookkeeping (not a safety check -- M4's analyzer
                // already proved both arms agree wherever it matters) --
                // lower each arm from the same starting snapshot so one
                // arm's moves/syncs don't leak into the other, then keep
                // the then-arm's resulting view (or the before-snapshot,
                // for an absent else, since that's the "didn't run" path).
                let domains_before = self.domains.clone();
                let then_body = self.lower_block(then_block, fn_name)?;
                let domains_after_then = std::mem::replace(&mut self.domains, domains_before.clone());
                let else_body = match else_block {
                    Some(b) => self.lower_block(b, fn_name)?,
                    None => {
                        self.domains = domains_before;
                        Vec::new()
                    }
                };
                if else_block.is_some() {
                    self.domains = domains_after_then;
                }
                out.push(PirInst::If {
                    cond: pir_cond,
                    then_body,
                    else_body,
                });
                Ok(())
            }

            Stmt::While { cond, body } => {
                let context = format!("`while` inside fn '{fn_name}'");
                let pir_cond = lower_pir_expr(cond, &context)?;
                // Same "ran zero or one times" model as M4: keep the
                // post-body view, matching an `if` with an implicit empty
                // else that merges into the then-arm's state.
                let lowered_body = self.lower_block(body, fn_name)?;
                out.push(PirInst::While {
                    cond: pir_cond,
                    body: lowered_body,
                });
                Ok(())
            }

            Stmt::Assign { name, value } => {
                let context = format!("assignment to '{name}' inside fn '{fn_name}'");
                let pir_value = lower_pir_expr(value, &context)?;
                out.push(PirInst::Assign {
                    name: name.clone(),
                    value: pir_value,
                });
                Ok(())
            }
        }
    }

    fn lower_var_decl(
        &mut self,
        name: &str,
        ty: Option<&TypeExpr>,
        domain: Option<Domain>,
        init: Option<&Expr>,
        out: &mut Vec<PirInst>,
    ) -> Result<(), PirError> {
        if let Some(d) = domain {
            self.domains.insert(name.to_string(), d);
        }

        match init {
            Some(Expr::Borrow { target, mode }) => {
                out.push(PirInst::EntityBorrow {
                    name: name.to_string(),
                    target: target.clone(),
                    mode: *mode,
                });
                if let Some(d) = self.domains.get(target).copied() {
                    self.domains.insert(name.to_string(), d);
                }
                Ok(())
            }
            Some(Expr::StructLit { fields, .. }) => {
                let ty = ty.ok_or_else(|| PirError::MissingType {
                    context: format!("'{name}'"),
                })?;
                let resolved_ty = self.resolve(ty)?;
                let mut lowered_fields = Vec::new();
                for (fname, fexpr) in fields {
                    lowered_fields.push((fname.clone(), lower_literal(fexpr, name)?));
                }
                out.push(PirInst::EntityCreateWithFields {
                    name: name.to_string(),
                    ty: resolved_ty,
                    domain,
                    fields: lowered_fields,
                });
                Ok(())
            }
            Some(Expr::Call { callee, args }) => {
                let ty = ty.ok_or_else(|| PirError::MissingType {
                    context: format!("'{name}' (initialized from a call; codegen needs an explicit type annotation)"),
                })?;
                let resolved_ty = self.resolve(ty)?;
                let lowered_args = self.lower_call_args(args, name)?;
                out.push(PirInst::CallAssign {
                    name: name.to_string(),
                    ty: resolved_ty,
                    domain,
                    callee: callee.clone(),
                    args: lowered_args,
                });
                Ok(())
            }
            Some(lit @ (Expr::IntLit(_) | Expr::FloatLit(_) | Expr::BoolLit(_) | Expr::StringLit(_))) => {
                let ty = ty.ok_or_else(|| PirError::MissingType {
                    context: format!("'{name}'"),
                })?;
                let resolved_ty = self.resolve(ty)?;
                let value = lower_literal(lit, name)?;
                out.push(PirInst::EntityCreateWithLiteral {
                    name: name.to_string(),
                    ty: resolved_ty,
                    domain,
                    value,
                });
                Ok(())
            }
            Some(other) => Err(PirError::UnsupportedExpr {
                context: format!("initializer for '{name}': {other:?}"),
            }),
            None => {
                let ty = ty.ok_or_else(|| PirError::MissingType {
                    context: format!("'{name}'"),
                })?;
                let resolved_ty = self.resolve(ty)?;
                out.push(PirInst::EntityCreate {
                    name: name.to_string(),
                    ty: resolved_ty,
                    domain,
                });
                Ok(())
            }
        }
    }

    fn lower_expr_stmt(&mut self, e: &Expr, out: &mut Vec<PirInst>) -> Result<(), PirError> {
        match e {
            Expr::Call { callee, args } => {
                let lowered_args = self.lower_call_args(args, callee)?;
                out.push(PirInst::Call {
                    callee: callee.clone(),
                    args: lowered_args,
                });
                Ok(())
            }
            other => Err(PirError::UnsupportedExpr {
                context: format!("statement expression: {other:?}"),
            }),
        }
    }

    fn lower_call_args(&self, args: &[Expr], context: &str) -> Result<Vec<CallArg>, PirError> {
        let mut out = Vec::new();
        for a in args {
            out.push(match a {
                Expr::Ident(n) => CallArg::Entity(n.clone()),
                Expr::IntLit(n) => CallArg::Literal(PirLiteral::Int(*n)),
                Expr::FloatLit(n) => CallArg::Literal(PirLiteral::Float(*n)),
                Expr::StringLit(s) => CallArg::Literal(PirLiteral::Str(s.clone())),
                Expr::BoolLit(b) => CallArg::Literal(PirLiteral::Bool(*b)),
                // M6: the M2 analyzer already guarantees this only appears
                // as the register argument to volatile_read/volatile_write,
                // and that its domain is @MMIO.
                Expr::DomainRef { name, .. } => CallArg::MmioRegister(name.clone()),
                other => {
                    return Err(PirError::UnsupportedExpr {
                        context: format!("argument to '{context}': {other:?}"),
                    })
                }
            });
        }
        Ok(out)
    }
}

/// Lower an `if`/`while` condition or a `name = expr;` (M9) right-hand
/// side to `PirExpr`. Identifiers, literals, binary comparisons/
/// arithmetic, and (M11) `volatile_read(REG @MMIO)` -- see the module doc
/// comment for why anything else is a clear error rather than a guess.
fn lower_pir_expr(e: &Expr, context: &str) -> Result<PirExpr, PirError> {
    match e {
        Expr::Ident(n) => Ok(PirExpr::Ident(n.clone())),
        Expr::IntLit(n) => Ok(PirExpr::IntLit(*n)),
        Expr::FloatLit(n) => Ok(PirExpr::FloatLit(*n)),
        Expr::BoolLit(b) => Ok(PirExpr::BoolLit(*b)),
        Expr::Binary { op, lhs, rhs } => Ok(PirExpr::Binary {
            op: *op,
            lhs: Box::new(lower_pir_expr(lhs, context)?),
            rhs: Box::new(lower_pir_expr(rhs, context)?),
        }),
        // M11: the M6 analyzer (`check_call`, invoked via `check_expr` on
        // every `if`/`while` condition and assignment RHS) already
        // guarantees a `volatile_read` call here has exactly one
        // @MMIO-tagged register argument -- same trust relationship PIR
        // already has with `lower_call_args`'s volatile_read handling.
        Expr::Call { callee, args } if callee == "volatile_read" => match args.as_slice() {
            [Expr::DomainRef { name, .. }] => Ok(PirExpr::VolatileRead {
                register: name.clone(),
            }),
            _ => Err(PirError::UnsupportedExpr {
                context: format!("malformed volatile_read in {context}"),
            }),
        },
        other => Err(PirError::UnsupportedExpr {
            context: format!("expression in {context}: {other:?}"),
        }),
    }
}

fn lower_literal(e: &Expr, context: &str) -> Result<PirLiteral, PirError> {
    match e {
        Expr::IntLit(n) => Ok(PirLiteral::Int(*n)),
        Expr::FloatLit(n) => Ok(PirLiteral::Float(*n)),
        Expr::StringLit(s) => Ok(PirLiteral::Str(s.clone())),
        Expr::BoolLit(b) => Ok(PirLiteral::Bool(*b)),
        other => Err(PirError::UnsupportedExpr {
            context: format!("struct literal field in '{context}': {other:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_src(src: &str) -> Result<PirProgram, PirError> {
        let program = phase_parser::parse(src).expect("fixture must parse");
        phase_analysis::analyze(&program).expect("fixture must pass analysis");
        build(&program)
    }

    #[test]
    fn simple_buffer_decl_and_move_lower_correctly() {
        let src = r#"
            fn main() {
                buffer<u8, 16> x @RAM;
                move x -> @DMA;
                sync(x);
            }
        "#;
        let pir = build_src(src).unwrap();
        let main = &pir.fns[0];
        assert_eq!(
            main.body,
            vec![
                PirInst::EntityCreate {
                    name: "x".into(),
                    ty: PirType::Buffer {
                        elem: Box::new(PirType::Scalar("uint8_t".into())),
                        len: 16
                    },
                    domain: Some(Domain::Ram),
                },
                PirInst::EntityMove {
                    name: "x".into(),
                    to: Domain::Dma
                },
                PirInst::EntitySync {
                    name: "x".into(),
                    from: Domain::Dma
                },
            ]
        );
    }

    #[test]
    fn entity_decl_lowers_to_field_list() {
        let src = "entity Sample : @RAM { value: f32 } fn main() {}";
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.entities[0],
            PirEntityDef {
                name: "Sample".into(),
                fields: vec![("value".into(), PirType::Scalar("float".into()))],
            }
        );
    }

    #[test]
    fn extern_fn_signature_lowers_with_buffer_param() {
        let src = "extern fn f(x: buffer<u8, 4> @RAM); fn main() {}";
        let pir = build_src(src).unwrap();
        assert_eq!(pir.extern_fns[0].name, "f");
        assert_eq!(
            pir.extern_fns[0].params[0].1,
            PirType::Buffer {
                elem: Box::new(PirType::Scalar("uint8_t".into())),
                len: 4
            }
        );
    }

    #[test]
    fn call_statement_lowers_with_entity_args() {
        let src = r#"
            extern fn f(a: buffer<u8, 4> @RAM, b: buffer<u8, 4> @RAM);
            fn main() {
                buffer<u8, 4> a @RAM;
                buffer<u8, 4> b @RAM;
                f(a, b);
            }
        "#;
        let pir = build_src(src).unwrap();
        let main = &pir.fns[0];
        assert_eq!(
            main.body[2],
            PirInst::Call {
                callee: "f".into(),
                args: vec![CallArg::Entity("a".into()), CallArg::Entity("b".into())],
            }
        );
    }

    #[test]
    fn struct_literal_init_lowers_to_entity_create_with_fields() {
        let src = "entity Sample : @RAM { value: f32 } fn main() { let s: Sample @RAM = Sample { value: 1.5 }; }";
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[0],
            PirInst::EntityCreateWithFields {
                name: "s".into(),
                ty: PirType::Named("Sample".into()),
                domain: Some(Domain::Ram),
                fields: vec![("value".into(), PirLiteral::Float(1.5))],
            }
        );
    }

    #[test]
    fn call_assign_requires_explicit_type() {
        // `let p1 = receive_packet();` has no explicit type -- codegen
        // can't yet infer one, so this is a clear, documented error, not a
        // panic or a silently wrong C type.
        let src = r#"
            extern fn receive_packet() -> u32;
            fn main() {
                let p1 = receive_packet();
            }
        "#;
        let err = build_src(src).unwrap_err();
        assert!(matches!(err, PirError::MissingType { .. }));
    }

    // ---- M8: if/while lowering ---------------------------------------------

    #[test]
    fn if_else_lowers_to_pir_if_with_both_arms() {
        let src = r#"
            extern fn tick();
            extern fn tock();
            fn main() {
                if flag {
                    tick();
                } else {
                    tock();
                }
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[0],
            PirInst::If {
                cond: PirExpr::Ident("flag".into()),
                then_body: vec![PirInst::Call {
                    callee: "tick".into(),
                    args: vec![],
                }],
                else_body: vec![PirInst::Call {
                    callee: "tock".into(),
                    args: vec![],
                }],
            }
        );
    }

    #[test]
    fn if_without_else_lowers_with_an_empty_else_body() {
        let src = r#"
            extern fn tick();
            fn main() {
                if flag {
                    tick();
                }
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[0],
            PirInst::If {
                cond: PirExpr::Ident("flag".into()),
                then_body: vec![PirInst::Call {
                    callee: "tick".into(),
                    args: vec![],
                }],
                else_body: vec![],
            }
        );
    }

    #[test]
    fn while_lowers_to_pir_while_with_a_comparison_condition() {
        let src = r#"
            extern fn tick();
            fn main() {
                let n: i32 = 0;
                while n < 3 {
                    tick();
                }
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[1],
            PirInst::While {
                cond: PirExpr::Binary {
                    op: BinOp::Lt,
                    lhs: Box::new(PirExpr::Ident("n".into())),
                    rhs: Box::new(PirExpr::IntLit(3)),
                },
                body: vec![PirInst::Call {
                    callee: "tick".into(),
                    args: vec![],
                }],
            }
        );
    }

    #[test]
    fn nested_if_inside_while_lowers_recursively() {
        let src = r#"
            extern fn tick();
            extern fn tock();
            fn main() {
                let n: i32 = 0;
                while n < 3 {
                    if n == 0 {
                        tick();
                    } else {
                        tock();
                    }
                }
            }
        "#;
        let pir = build_src(src).unwrap();
        match &pir.fns[0].body[1] {
            PirInst::While { body, .. } => {
                assert_eq!(body.len(), 1);
                assert!(matches!(body[0], PirInst::If { .. }));
            }
            other => panic!("expected While, got {other:?}"),
        }
    }

    // ---- M9: assignment ----------------------------------------------------

    #[test]
    fn assignment_lowers_to_pir_assign() {
        let src = r#"
            fn main() {
                let n: i32 = 0;
                n = n + 1;
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[1],
            PirInst::Assign {
                name: "n".into(),
                value: PirExpr::Binary {
                    op: BinOp::Add,
                    lhs: Box::new(PirExpr::Ident("n".into())),
                    rhs: Box::new(PirExpr::IntLit(1)),
                },
            }
        );
    }

    #[test]
    fn assignment_inside_while_body_lowers_recursively() {
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
        let pir = build_src(src).unwrap();
        match &pir.fns[0].body[1] {
            PirInst::While { body, .. } => {
                assert_eq!(body.len(), 2);
                assert!(matches!(body[1], PirInst::Assign { .. }));
            }
            other => panic!("expected While, got {other:?}"),
        }
    }

    // ---- M11: volatile_read in conditions -----------------------------------

    #[test]
    fn volatile_read_in_while_condition_lowers_to_volatile_read_expr() {
        let src = r#"
            extern fn arm_if_ready();
            fn main() {
                while volatile_read(READY @MMIO) == 0 {
                    arm_if_ready();
                }
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[0],
            PirInst::While {
                cond: PirExpr::Binary {
                    op: BinOp::Eq,
                    lhs: Box::new(PirExpr::VolatileRead {
                        register: "READY".into(),
                    }),
                    rhs: Box::new(PirExpr::IntLit(0)),
                },
                body: vec![PirInst::Call {
                    callee: "arm_if_ready".into(),
                    args: vec![],
                }],
            }
        );
    }

    #[test]
    fn volatile_read_in_while_condition_is_collected_as_an_mmio_register() {
        // The bug this test guards against: `collect_mmio_registers` only
        // scanning top-level CallAssign/Call args (pre-M11) would miss a
        // register that's only ever referenced from inside a `while`
        // condition, and codegen would emit `while (READY == 0)` with no
        // matching `volatile uint32_t READY;` declaration anywhere.
        let src = r#"
            fn main() {
                while volatile_read(READY @MMIO) == 0 {
                }
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(pir.mmio_registers, vec!["READY".to_string()]);
    }

    #[test]
    fn volatile_read_in_if_condition_inside_while_body_is_still_collected() {
        // Nested: an MMIO register referenced only inside an `if` that's
        // itself inside a `while` body -- exercises the recursive walk,
        // not just one level of nesting.
        let src = r#"
            extern fn tick();
            fn main() {
                let n: i32 = 0;
                while n < 3 {
                    if volatile_read(FLAG @MMIO) == 1 {
                        tick();
                    }
                    n = n + 1;
                }
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(pir.mmio_registers, vec!["FLAG".to_string()]);
    }

    #[test]
    fn volatile_read_as_assignment_rhs_lowers_correctly() {
        let src = r#"
            fn main() {
                let status: u32 = 0;
                status = volatile_read(STATUS @MMIO);
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[1],
            PirInst::Assign {
                name: "status".into(),
                value: PirExpr::VolatileRead {
                    register: "STATUS".into(),
                },
            }
        );
    }

    #[test]
    fn general_extern_fn_call_in_a_condition_is_still_a_clear_error() {
        // M11 deliberately only allows volatile_read, not arbitrary calls,
        // inside a condition -- see the PirExpr::VolatileRead doc comment.
        let src = r#"
            extern fn is_ready() -> bool;
            fn main() {
                while is_ready() {
                }
            }
        "#;
        let err = build_src(src).unwrap_err();
        assert!(matches!(err, PirError::UnsupportedExpr { .. }));
    }

    #[test]
    fn unknown_type_is_a_clear_error() {
        let src = "fn main() { let x: Ghost @RAM = Ghost { a: 1 }; }";
        let err = build_src(src).unwrap_err();
        assert!(matches!(err, PirError::UnknownType { name } if name == "Ghost"));
    }

    #[test]
    fn radio_pipeline_shaped_program_lowers_end_to_end() {
        let src = r#"
            entity Sample : @RAM { value: f32 }
            extern fn fir_filter(in_: buffer<Sample, 4> @RAM, out: buffer<Sample, 4> @RAM);
            extern fn device_capture(dst: buffer<Sample, 4> @DMA);
            extern fn device_playback(src: buffer<Sample, 4> @DEVICE);
            fn main() {
                buffer<Sample, 4> raw @DMA;
                device_capture(raw);
                sync(raw);
                buffer<Sample, 4> filtered @RAM;
                fir_filter(raw, filtered);
                move filtered -> @DEVICE;
                device_playback(filtered);
                sync(filtered);
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(pir.entities.len(), 1);
        assert_eq!(pir.extern_fns.len(), 3);
        assert_eq!(pir.fns[0].body.len(), 8);
        // spot-check the sync-domain bookkeeping is correct for both syncs
        assert!(matches!(
            pir.fns[0].body[2],
            PirInst::EntitySync { from: Domain::Dma, .. }
        ));
        assert!(matches!(
            pir.fns[0].body[7],
            PirInst::EntitySync { from: Domain::Device, .. }
        ));
    }

    // ---- M6: volatile MMIO register access ---------------------------------

    #[test]
    fn volatile_read_lowers_to_call_assign_with_mmio_register_arg() {
        let src = r#"
            fn main() {
                let status: u32 = volatile_read(UART_STATUS @MMIO);
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[0],
            PirInst::CallAssign {
                name: "status".into(),
                ty: PirType::Scalar("uint32_t".into()),
                domain: None,
                callee: "volatile_read".into(),
                args: vec![CallArg::MmioRegister("UART_STATUS".into())],
            }
        );
    }

    #[test]
    fn volatile_write_lowers_to_call_with_mmio_register_and_literal_args() {
        let src = r#"
            fn main() {
                volatile_write(UART_CONTROL @MMIO, 1);
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(
            pir.fns[0].body[0],
            PirInst::Call {
                callee: "volatile_write".into(),
                args: vec![
                    CallArg::MmioRegister("UART_CONTROL".into()),
                    CallArg::Literal(PirLiteral::Int(1)),
                ],
            }
        );
    }

    #[test]
    fn distinct_mmio_registers_are_collected_once_each_in_first_seen_order() {
        let src = r#"
            fn main() {
                let a: u32 = volatile_read(REG_A @MMIO);
                let b: u32 = volatile_read(REG_B @MMIO);
                volatile_write(REG_A @MMIO, a);
            }
        "#;
        let pir = build_src(src).unwrap();
        assert_eq!(pir.mmio_registers, vec!["REG_A".to_string(), "REG_B".to_string()]);
    }

    #[test]
    fn program_with_no_volatile_access_has_no_mmio_registers() {
        let pir = build_src("fn main() {}").unwrap();
        assert!(pir.mmio_registers.is_empty());
    }
}
