//! Abstract Syntax Tree definitions for PHASE (v0.1, M1 scope).
//!
//! M1 covers: entity declarations, functions (incl. `extern fn`), typestate
//! declarations (parsed but not yet analyzed), and straight-line statements
//! (`let`, buffer decls, `move`, `sync`, `borrow`/`release`, expression
//! statements, `return`). No `if`/`while` yet — that's M4.

/// The four physical domains supported in v0.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Domain {
    Ram,
    Stack,
    Dma,
    Mmio,
    Device,
}

impl Domain {
    pub fn from_name(name: &str) -> Option<Domain> {
        match name {
            "RAM" => Some(Domain::Ram),
            "STACK" => Some(Domain::Stack),
            "DMA" => Some(Domain::Dma),
            "MMIO" => Some(Domain::Mmio),
            "DEVICE" => Some(Domain::Device),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Domain::Ram => "RAM",
            Domain::Stack => "STACK",
            Domain::Dma => "DMA",
            Domain::Mmio => "MMIO",
            Domain::Device => "DEVICE",
        }
    }
}

/// A type expression: either a named type (`Sample`, `f32`, `i16`, ...),
/// a fixed-size buffer of some element type (`buffer<Sample, 1024>`), or a
/// typestate-parameterized type (`Packet<Received>`).
#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    Named(String),
    Buffer { elem: Box<TypeExpr>, len: u64 },
    /// `Packet<Received>` — `name` is the `state`-declared machine name
    /// (`Packet`), `state` is the specific state (`Received`).
    Stateful { name: String, state: String },
}

impl TypeExpr {
    /// The base type name, ignoring any typestate parameter — used to look
    /// up a default domain the same way for `Sample` and `Packet<Received>`
    /// alike. `None` for `buffer<...>`, which has no single base name.
    pub fn base_name(&self) -> Option<&str> {
        match self {
            TypeExpr::Named(n) => Some(n),
            TypeExpr::Stateful { name, .. } => Some(name),
            TypeExpr::Buffer { .. } => None,
        }
    }
}

/// Borrow mode used by `borrow x read` / `borrow x write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowMode {
    Read,
    Write,
}

/// A field in an `entity` declaration: `value: f32`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeExpr,
}

/// A function parameter: `input: buffer<Sample, 1024> @RAM`.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: TypeExpr,
    pub domain: Option<Domain>,
}

/// `entity Sample : @RAM { value: f32 }`
#[derive(Debug, Clone, PartialEq)]
pub struct EntityDecl {
    pub name: String,
    pub domain: Option<Domain>,
    pub fields: Vec<FieldDecl>,
}

/// One step of a typestate chain, e.g. `Received -> Decoded -> Validated`.
/// Parsed in M1, analyzed starting M3.
#[derive(Debug, Clone, PartialEq)]
pub struct StateDecl {
    pub name: String,
    pub states: Vec<String>,
}

/// `fn main() { ... }`
#[derive(Debug, Clone, PartialEq)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Block,
}

/// `extern fn fir_filter(input: buffer<Sample,1024> @RAM, ...);`
/// Trusted, opaque operation — no body, just a signature.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternFnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    Entity(EntityDecl),
    State(StateDecl),
    Fn(FnDecl),
    ExternFn(ExternFnDecl),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let s: Sample @RAM = Sample { value: 0.0 };`
    /// `buffer<Sample, 1024> radio_buf @DMA;`
    /// Both forms unify to one AST node: a named local with an optional
    /// declared type/domain and an optional initializer.
    VarDecl {
        name: String,
        ty: Option<TypeExpr>,
        domain: Option<Domain>,
        init: Option<Expr>,
    },
    /// `move radio_buf -> @DMA;`
    Move { name: String, to: Domain },
    /// `sync(radio_buf);`
    Sync { name: String },
    /// `release view;`
    Release { name: String },
    /// `destroy x;`
    Destroy { name: String },
    /// A bare expression statement, e.g. a function call: `fir_filter(a, b);`
    Expr(Expr),
    /// `return expr?;`
    Return(Option<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Ident(String),
    IntLit(i64),
    FloatLit(f64),
    StringLit(String),
    BoolLit(bool),
    /// `fir_filter(raw, filtered)`
    Call { callee: String, args: Vec<Expr> },
    /// `Sample { value: 0.0 }`
    StructLit {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    /// `borrow radio_buf read`
    Borrow { target: String, mode: BorrowMode },
    /// `UART_STATUS @MMIO` — a name tagged with an explicit domain,
    /// used as an argument to volatile_read/volatile_write.
    DomainRef { name: String, domain: Domain },
    /// `a.b`
    FieldAccess { base: Box<Expr>, field: String },
    /// `lhs op rhs`
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub items: Vec<Item>,
}
