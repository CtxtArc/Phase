//! Abstract Syntax Tree definitions for PHASE: entities, typestate
//! machines, functions and `extern fn` signatures, and the statement and
//! expression forms (including `if`/`while` and scalar assignment).

/// A region of source text: from `(line, col)` up to, but not including,
/// `(end_line, end_col)`. Lines and columns are 1-based and count chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Span {
    pub line: u32,
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Span {
    /// The smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        let (start, _) = if (self.line, self.col) <= (other.line, other.col) {
            (self, other)
        } else {
            (other, self)
        };
        let end = if (self.end_line, self.end_col) >= (other.end_line, other.end_col) {
            self
        } else {
            other
        };
        Span {
            line: start.line,
            col: start.col,
            end_line: end.end_line,
            end_col: end.end_col,
        }
    }
}

/// A syntax node paired with the source span it was parsed from. Derefs to
/// the node, so most code can use it as if it were the node itself.
#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

impl<T> std::ops::Deref for Spanned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.node
    }
}

/// The five physical domains (spec §2.1).
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
    Buffer {
        elem: Box<TypeExpr>,
        len: u64,
    },
    /// `Packet<Received>` — `name` is the `state`-declared machine name
    /// (`Packet`), `state` is the specific state (`Received`).
    Stateful {
        name: String,
        state: String,
    },
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
    /// `read` / `write` before the type: how the function may access the
    /// argument's contents. `None` (no mode) is the pre-1.2 default.
    pub access: Option<BorrowMode>,
    pub ty: TypeExpr,
    /// The domain the argument must be in when the function is called.
    pub domain: Option<Domain>,
    /// `Some(to)` for `name: T @FROM -> @TO`: the function leaves the
    /// argument in `to` instead of handing it back in `domain`.
    pub domain_after: Option<Domain>,
    pub span: Span,
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
    /// Span of the function's name in `fn name(..)`.
    pub name_span: Span,
    pub params: Vec<Param>,
    pub return_type: Option<TypeExpr>,
    pub body: Block,
}

/// `extern fn fir_filter(input: buffer<Sample,1024> @RAM, ...);`
/// Trusted, opaque operation — no body, just a signature.
#[derive(Debug, Clone, PartialEq)]
pub struct ExternFnDecl {
    pub name: String,
    /// Span of the function's name in `extern fn name(..)`.
    pub name_span: Span,
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
    pub stmts: Vec<Spanned<Stmt>>,
    /// Span of the closing `}` -- where falling off the end of a function
    /// body is reported.
    pub close: Span,
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
    /// `swap a, b;` (v1.2) -- exchange the storage two buffer names refer
    /// to, along with everything known about it (domain, history). The
    /// building block for ping-pong double buffering.
    Swap { a: String, b: String },
    /// A bare expression statement, e.g. a function call: `fir_filter(a, b);`
    Expr(Expr),
    /// `return expr?;`
    Return(Option<Expr>),
    /// `if cond { .. } else { .. }` — `else_block` is `None` when there's
    /// no `else` clause. `else if` chains desugar to a single nested `If`
    /// as the sole statement of the (implicit) else block.
    If {
        cond: Expr,
        then_block: Block,
        else_block: Option<Block>,
    },
    /// `while cond { .. }`
    While { cond: Expr, body: Block },
    /// `name = expr;` (M9) -- reassigns an already-declared scalar local.
    /// Only a bare identifier target; no field/index lvalues.
    Assign { name: String, value: Expr },
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
    Call {
        callee: String,
        args: Vec<Expr>,
    },
    /// `Sample { value: 0.0 }`
    StructLit {
        name: String,
        fields: Vec<(String, Expr)>,
    },
    /// `borrow radio_buf read`
    Borrow {
        target: String,
        mode: BorrowMode,
    },
    /// `UART_STATUS @MMIO` — a name tagged with an explicit domain,
    /// used as an argument to volatile_read/volatile_write.
    DomainRef {
        name: String,
        domain: Domain,
    },
    /// `a.b`
    FieldAccess {
        base: Box<Expr>,
        field: String,
    },
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
