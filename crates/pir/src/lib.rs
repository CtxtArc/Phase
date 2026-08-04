//! PHASE M5: PIR — the Phase Intermediate Representation (spec §7).
//!
//! PIR sits between the (already-analyzed) AST and C codegen. It flattens
//! each function into an ordered list of instructions and resolves every
//! type down to something codegen can turn directly into C.
//!
//! Scope (documented, not silent): M5 targets the straight-line subset --
//! a body containing `if`/`while` is rejected here with a clear
//! `UnsupportedControlFlow` error rather than silently mis-compiled.
//! Lowering control flow to PIR (real branches / phi-materialization in C)
//! is future work; M4's branch-merge analyzer already proves such bodies
//! are *safe*, this crate just doesn't yet know how to generate code for
//! them. `phase build` is only guaranteed to work on straight-line
//! programs like `examples/radio_pipeline.phase`.
//!
//! PIR generation trusts that the program has already passed
//! `phase_analysis::analyze` — it does not re-derive domain/borrow/
//! typestate safety, only enough domain bookkeeping (which domain an
//! entity is in right now) to pick the correct runtime call for `sync`.

use phase_ast::{BorrowMode, Domain, Expr, Item, Program, Stmt, TypeExpr};
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

#[derive(Debug, Clone, PartialEq)]
pub enum CallArg {
    Entity(String),
    Literal(PirLiteral),
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

    Ok(out)
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

            Stmt::If { .. } => Err(PirError::UnsupportedControlFlow {
                context: format!("`if` inside fn '{fn_name}'"),
            }),
            Stmt::While { .. } => Err(PirError::UnsupportedControlFlow {
                context: format!("`while` inside fn '{fn_name}'"),
            }),
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

    #[test]
    fn if_is_rejected_with_a_clear_unsupported_error() {
        let src = r#"
            fn main() {
                buffer<u8, 4> x @RAM;
                if flag {
                    move x -> @DMA;
                } else {
                    move x -> @DMA;
                }
            }
        "#;
        let err = build_src(src).unwrap_err();
        assert!(matches!(err, PirError::UnsupportedControlFlow { .. }));
    }

    #[test]
    fn while_is_rejected_with_a_clear_unsupported_error() {
        let src = r#"
            extern fn tick();
            fn main() {
                while flag {
                    tick();
                }
            }
        "#;
        let err = build_src(src).unwrap_err();
        assert!(matches!(err, PirError::UnsupportedControlFlow { .. }));
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
}
