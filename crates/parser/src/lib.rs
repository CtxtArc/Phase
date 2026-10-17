//! Hand-written recursive-descent parser for PHASE: tokens -> AST.

use phase_ast::*;
use phase_lexer::{tokenize_file, LexError, Span, Token, TokenKind};

mod load;
pub use load::{load, LoadError, Loaded, SourceFile};

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "parse error at {}:{}: {}",
            self.span.line, self.span.col, self.message
        )
    }
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError {
            message: e.message,
            span: e.span,
        }
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    /// When true, a bare `Ident {` is *not* parsed as a struct literal --
    /// used while parsing `if`/`while` conditions, where `{` instead opens
    /// the following block (the same ambiguity Rust resolves the same way).
    restrict_struct_lit: bool,
    /// Span of the most recently consumed token -- the end of whatever
    /// construct was just parsed.
    prev_span: Span,
}

/// Parse a full PHASE source file into a `Program`.
pub fn parse(src: &str) -> Result<Program, ParseError> {
    parse_file(src, 0)
}

/// Like [`parse`], with every span in source file `file`. `import`s are
/// left in the program; [`load`] resolves them.
pub fn parse_file(src: &str, file: u32) -> Result<Program, ParseError> {
    let tokens = tokenize_file(src, file)?;
    Parser::new(tokens).parse_program()
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser {
            tokens,
            pos: 0,
            restrict_struct_lit: false,
            prev_span: Span::default(),
        }
    }

    // ---- token stream helpers ----------------------------------------

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn peek_at(&self, offset: usize) -> &TokenKind {
        let idx = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        self.prev_span = t.span;
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token, ParseError> {
        if self.peek() == &kind {
            Ok(self.bump())
        } else {
            Err(ParseError {
                message: format!("expected {:?}, found {:?}", kind, self.peek()),
                span: self.peek_span(),
            })
        }
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            TokenKind::Ident(s) => {
                self.bump();
                Ok(s)
            }
            other => Err(ParseError {
                message: format!("expected identifier, found {:?}", other),
                span: self.peek_span(),
            }),
        }
    }

    /// `@DMA` -> Domain::Dma
    fn parse_domain(&mut self) -> Result<Domain, ParseError> {
        let span = self.peek_span();
        self.expect(TokenKind::At)?;
        let name = self.expect_ident()?;
        Domain::from_name(&name).ok_or_else(|| ParseError {
            message: format!("unknown domain '@{name}'"),
            span,
        })
    }

    fn opt_domain(&mut self) -> Result<Option<Domain>, ParseError> {
        if self.at(&TokenKind::At) {
            Ok(Some(self.parse_domain()?))
        } else {
            Ok(None)
        }
    }

    // ---- top level -----------------------------------------------------

    pub fn parse_program(&mut self) -> Result<Program, ParseError> {
        let mut program = self.parse_items()?;
        phase_ast::resolve_irq_names(&mut program);
        Ok(program)
    }

    fn parse_items(&mut self) -> Result<Program, ParseError> {
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            items.push(self.parse_item()?);
        }
        Ok(Program { items })
    }

    fn parse_item(&mut self) -> Result<Item, ParseError> {
        match self.peek() {
            TokenKind::KwEntity => self.parse_entity_decl().map(Item::Entity),
            TokenKind::KwState => self.parse_state_decl().map(Item::State),
            TokenKind::KwExtern => self.parse_extern_fn_decl().map(Item::ExternFn),
            TokenKind::KwFn => {
                let f = self.parse_fn_decl()?;
                if let Some((_, span)) = f.irq {
                    return Err(ParseError {
                        message: format!(
                            "only an interrupt handler has an IRQ: `interrupt fn {}() @ irq N`",
                            f.name
                        ),
                        span,
                    });
                }
                Ok(Item::Fn(f))
            }
            // `device` is a keyword only here, at the start of an item, so
            // existing programs may keep using it as a name.
            TokenKind::Ident(w) if w == "device" => self.parse_device_item(),
            TokenKind::Ident(w) if w == "interrupt" => self.parse_interrupt_fn().map(Item::Fn),
            // v2.4: `const NAME: u32 = expr;` -- a keyword only here.
            TokenKind::Ident(w)
                if w == "const"
                    && matches!(self.peek_at(1), TokenKind::Ident(_))
                    && matches!(self.peek_at(2), TokenKind::Colon) =>
            {
                self.bump();
                let span = self.peek_span();
                let name = self.expect_ident()?;
                self.expect(TokenKind::Colon)?;
                let ty = self.expect_ident()?;
                self.expect(TokenKind::Eq)?;
                let value = self.parse_expr()?;
                self.expect(TokenKind::Semi)?;
                Ok(Item::Const(ConstDecl {
                    name,
                    span,
                    ty,
                    value,
                }))
            }
            TokenKind::Ident(w) if w == "event" => {
                self.bump();
                let span = self.peek_span();
                let name = self.expect_ident()?;
                self.expect(TokenKind::Semi)?;
                Ok(Item::Event(EventDecl { name, span }))
            }
            TokenKind::Ident(w) if w == "import" => {
                let start = self.peek_span();
                self.bump();
                let path = match self.peek().clone() {
                    TokenKind::Str(s) => {
                        self.bump();
                        s
                    }
                    other => {
                        return Err(ParseError {
                            message: format!(
                                "expected a file path in quotes after `import`, found {other:?}"
                            ),
                            span: self.peek_span(),
                        })
                    }
                };
                self.expect(TokenKind::Semi)?;
                Ok(Item::Import(ImportDecl {
                    path,
                    span: start.to(self.prev_span),
                }))
            }
            other => Err(ParseError {
                message: format!(
                    "expected item (entity/state/fn/extern fn/interrupt fn/event/device/import), found {:?}",
                    other
                ),
                span: self.peek_span(),
            }),
        }
    }

    /// A non-negative integer literal (decimal or hex), e.g. an address.
    fn expect_uint(&mut self, what: &str) -> Result<u64, ParseError> {
        match self.peek().clone() {
            TokenKind::Int(n) if n >= 0 => {
                self.bump();
                Ok(n as u64)
            }
            other => Err(ParseError {
                message: format!("expected {what} (an integer), found {other:?}"),
                span: self.peek_span(),
            }),
        }
    }

    /// The identifier `word`, which is a keyword only in this position.
    fn expect_word(&mut self, word: &str) -> Result<(), ParseError> {
        match self.peek() {
            TokenKind::Ident(w) if w == word => {
                self.bump();
                Ok(())
            }
            other => Err(ParseError {
                message: format!("expected '{word}', found {other:?}"),
                span: self.peek_span(),
            }),
        }
    }

    /// ```text
    /// device Uart0 @ 0x40004000 {
    ///     DATA:  read write u32 @ 0x00;
    ///     STATE: read u32 @ 0x04 { TXFULL: bit 0, RXFULL: bit 1 }
    ///     CTRL:  read write u32 @ 0x08 = 0 { TX_EN: bit 0, BAUD: bits 4..=11 }
    /// }
    /// ```
    /// `device NAME @ BASE { REGS }`, `device NAME: TYPE @ BASE;`, or
    /// `device type NAME { REGS }`.
    fn parse_device_item(&mut self) -> Result<Item, ParseError> {
        self.expect_word("device")?;
        // `device type X {` declares a layout; a device may still be
        // *named* `type` (`device type @ ..`).
        if matches!(self.peek(), TokenKind::Ident(w) if w == "type")
            && matches!(self.peek_at(1), TokenKind::Ident(_))
        {
            self.bump();
            let span = self.peek_span();
            let name = self.expect_ident()?;
            let regs = self.parse_reg_block()?;
            return Ok(Item::DeviceType(DeviceTypeDecl { name, span, regs }));
        }
        let name_span = self.peek_span();
        let name = self.expect_ident()?;
        let of_type = if self.at(&TokenKind::Colon) {
            self.bump();
            let tspan = self.peek_span();
            Some((self.expect_ident()?, tspan))
        } else {
            None
        };
        self.expect(TokenKind::At)?;
        let base = self.expect_uint("a base address")?;
        let regs = if of_type.is_some() {
            self.expect(TokenKind::Semi)?;
            Vec::new()
        } else {
            self.parse_reg_block()?
        };
        Ok(Item::Device(DeviceDecl {
            name,
            span: name_span,
            base,
            regs,
            of_type,
        }))
    }

    fn parse_reg_block(&mut self) -> Result<Vec<RegDecl>, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut regs = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            regs.push(self.parse_reg_decl()?);
        }
        self.expect(TokenKind::RBrace)?;
        Ok(regs)
    }

    fn parse_reg_decl(&mut self) -> Result<RegDecl, ParseError> {
        let span = self.peek_span();
        let name = self.expect_ident()?;
        // v2.4: `NAME[count]` -- an array of registers.
        let count = if self.at(&TokenKind::LBracket) {
            self.bump();
            let n = self.expect_uint("a register count")?;
            self.expect(TokenKind::RBracket)?;
            Some(n)
        } else {
            None
        };
        self.expect(TokenKind::Colon)?;
        let mut readable = false;
        let mut writable = false;
        loop {
            match self.peek() {
                TokenKind::KwRead if !readable => readable = true,
                TokenKind::KwWrite if !writable => writable = true,
                _ => break,
            }
            self.bump();
        }
        if !readable && !writable {
            return Err(ParseError {
                message: format!(
                    "register '{name}' needs an access mode: `read`, `write`, or `read write`"
                ),
                span: self.peek_span(),
            });
        }
        let ty = self.expect_ident()?;
        self.expect(TokenKind::At)?;
        let offset = self.expect_uint("a register offset")?;
        // `step N`: the distance between elements of an array (default:
        // the register's own size).
        let step = if matches!(self.peek(), TokenKind::Ident(w) if w == "step") {
            self.bump();
            Some(self.expect_uint("a step in bytes")?)
        } else {
            None
        };
        if step.is_some() && count.is_none() {
            return Err(ParseError {
                message: format!(
                    "`step` is for arrays of registers; declare '{name}[N]' to make one"
                ),
                span,
            });
        }
        let array = count.map(|n| {
            let size = match ty.as_str() {
                "u8" => 1,
                "u16" => 2,
                _ => 4,
            };
            (n.min(u32::MAX as u64) as u32, step.unwrap_or(size))
        });
        let reset = if self.at(&TokenKind::Eq) {
            self.bump();
            Some(self.expect_uint("a reset value")?)
        } else {
            None
        };
        // v2.4: `address` -- it holds a DMA address.
        let address = matches!(self.peek(), TokenKind::Ident(w) if w == "address");
        if address {
            self.bump();
        }
        let mut fields = Vec::new();
        if self.at(&TokenKind::LBrace) {
            self.bump();
            while !self.at(&TokenKind::RBrace) {
                let fspan = self.peek_span();
                let fname = self.expect_ident()?;
                self.expect(TokenKind::Colon)?;
                let (lo, hi) = match self.peek() {
                    TokenKind::Ident(w) if w == "bit" => {
                        self.bump();
                        let b = self.expect_uint("a bit number")?;
                        (b, b)
                    }
                    TokenKind::Ident(w) if w == "bits" => {
                        self.bump();
                        let lo = self.expect_uint("a bit number")?;
                        self.expect(TokenKind::DotDotEq)?;
                        let hi = self.expect_uint("a bit number")?;
                        (lo, hi)
                    }
                    other => {
                        return Err(ParseError {
                            message: format!(
                                "expected `bit N` or `bits LO..=HI` for field '{fname}', found {other:?}"
                            ),
                            span: self.peek_span(),
                        })
                    }
                };
                // v2.4: what reading and writing it does.
                let kind = match self.peek() {
                    TokenKind::Ident(w) if w == "w1c" => FieldKind::W1c,
                    TokenKind::Ident(w) if w == "rc" => FieldKind::Rc,
                    TokenKind::Ident(w) if w == "rc_w0" => FieldKind::RcW0,
                    _ => FieldKind::Plain,
                };
                if kind != FieldKind::Plain {
                    self.bump();
                }
                fields.push(RegField {
                    name: fname,
                    span: fspan,
                    lo: lo.min(u32::MAX as u64) as u32,
                    hi: hi.min(u32::MAX as u64) as u32,
                    kind,
                });
                if self.at(&TokenKind::Comma) {
                    self.bump();
                }
            }
            self.expect(TokenKind::RBrace)?;
        } else {
            self.expect(TokenKind::Semi)?;
        }
        Ok(RegDecl {
            name,
            span,
            readable,
            writable,
            ty,
            offset,
            reset,
            fields,
            array,
            address,
        })
    }

    fn parse_entity_decl(&mut self) -> Result<EntityDecl, ParseError> {
        self.expect(TokenKind::KwEntity)?;
        let name = self.expect_ident()?;
        let domain = if self.at(&TokenKind::Colon) {
            self.bump();
            Some(self.parse_domain()?)
        } else {
            None
        };
        self.expect(TokenKind::LBrace)?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            let fname = self.expect_ident()?;
            self.expect(TokenKind::Colon)?;
            let ty = self.parse_type_expr()?;
            fields.push(FieldDecl { name: fname, ty });
            if self.at(&TokenKind::Comma) {
                self.bump();
            }
        }
        self.expect(TokenKind::RBrace)?;
        Ok(EntityDecl {
            name,
            domain,
            fields,
        })
    }

    fn parse_state_decl(&mut self) -> Result<StateDecl, ParseError> {
        self.expect(TokenKind::KwState)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::LBrace)?;
        let mut states = vec![self.expect_ident()?];
        while self.at(&TokenKind::Arrow) {
            self.bump();
            states.push(self.expect_ident()?);
        }
        self.expect(TokenKind::RBrace)?;
        Ok(StateDecl { name, states })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, ParseError> {
        self.expect(TokenKind::LParen)?;
        let mut params = Vec::new();
        while !self.at(&TokenKind::RParen) {
            let start = self.peek_span();
            let name = self.expect_ident()?;
            self.expect(TokenKind::Colon)?;
            let access = match self.peek() {
                TokenKind::KwRead => {
                    self.bump();
                    Some(BorrowMode::Read)
                }
                TokenKind::KwWrite => {
                    self.bump();
                    Some(BorrowMode::Write)
                }
                _ => None,
            };
            let ty = self.parse_type_expr()?;
            let domain = self.opt_domain()?;
            // `@FROM -> @TO`: a domain change the function performs on this
            // argument. Unambiguous with a return type, which only follows
            // the closing `)`.
            let domain_after = if self.at(&TokenKind::Arrow) {
                if domain.is_none() {
                    return Err(ParseError {
                        message: format!(
                            "parameter '{name}' has `->` but no starting domain; write `{name}: <type> @FROM -> @TO`"
                        ),
                        span: self.peek_span(),
                    });
                }
                self.bump();
                Some(self.parse_domain()?)
            } else {
                None
            };
            params.push(Param {
                name,
                access,
                ty,
                domain,
                domain_after,
                span: start.to(self.prev_span),
            });
            if self.at(&TokenKind::Comma) {
                self.bump();
            } else {
                break;
            }
        }
        self.expect(TokenKind::RParen)?;
        Ok(params)
    }

    fn parse_return_type(&mut self) -> Result<Option<TypeExpr>, ParseError> {
        if self.at(&TokenKind::Arrow) {
            self.bump();
            Ok(Some(self.parse_type_expr()?))
        } else {
            Ok(None)
        }
    }

    fn parse_fn_decl(&mut self) -> Result<FnDecl, ParseError> {
        self.expect(TokenKind::KwFn)?;
        let name_span = self.peek_span();
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let return_type = self.parse_return_type()?;
        let mut irq_name = None;
        let irq = if self.at(&TokenKind::At) {
            // `interrupt fn h() @ irq N`, or `@ irq NAME` (a constant,
            // v3.0); the caller checked `interrupt`.
            self.bump();
            self.expect_word("irq")?;
            let span = self.peek_span();
            if let TokenKind::Ident(name) = self.peek().clone() {
                self.bump();
                irq_name = Some(name);
                Some((0, span))
            } else {
                let n = self.expect_uint("an IRQ number or a constant")?;
                Some((n.min(u32::MAX as u64) as u32, span))
            }
        } else {
            None
        };
        let body = self.parse_block()?;
        Ok(FnDecl {
            name,
            name_span,
            params,
            return_type,
            body,
            irq,
            irq_name,
        })
    }

    /// `interrupt fn NAME() @ irq N { .. }` (v2.2).
    fn parse_interrupt_fn(&mut self) -> Result<FnDecl, ParseError> {
        self.expect_word("interrupt")?;
        if !self.at(&TokenKind::KwFn) {
            return Err(ParseError {
                message: format!("expected `fn` after `interrupt`, found {:?}", self.peek()),
                span: self.peek_span(),
            });
        }
        let f = self.parse_fn_decl()?;
        if f.irq.is_none() {
            return Err(ParseError {
                message: format!(
                    "interrupt handler '{}' needs the IRQ it handles: `interrupt fn {}() @ irq N`",
                    f.name, f.name
                ),
                span: f.name_span,
            });
        }
        Ok(f)
    }

    fn parse_extern_fn_decl(&mut self) -> Result<ExternFnDecl, ParseError> {
        self.expect(TokenKind::KwExtern)?;
        self.expect(TokenKind::KwFn)?;
        let name_span = self.peek_span();
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let return_type = self.parse_return_type()?;
        self.expect(TokenKind::Semi)?;
        Ok(ExternFnDecl {
            name,
            name_span,
            params,
            return_type,
        })
    }

    // ---- types -----------------------------------------------------------

    fn parse_type_expr(&mut self) -> Result<TypeExpr, ParseError> {
        if self.at(&TokenKind::KwBuffer) {
            self.bump();
            self.expect(TokenKind::LAngle)?;
            let elem = self.parse_type_expr()?;
            self.expect(TokenKind::Comma)?;
            let len_span = self.peek_span();
            let len = match self.bump().kind {
                TokenKind::Int(n) if n >= 0 => BufferLen::Fixed(n as u64),
                TokenKind::Ident(s) if s == "_" => BufferLen::Any,
                other => {
                    return Err(ParseError {
                        message: format!(
                            "expected a non-negative integer buffer length or `_`, found {:?}",
                            other
                        ),
                        span: len_span,
                    })
                }
            };
            self.expect(TokenKind::RAngle)?;
            Ok(TypeExpr::Buffer {
                elem: Box::new(elem),
                len,
            })
        } else {
            let name = self.expect_ident()?;
            if self.at(&TokenKind::LAngle) {
                self.bump();
                let state = self.expect_ident()?;
                self.expect(TokenKind::RAngle)?;
                Ok(TypeExpr::Stateful { name, state })
            } else {
                Ok(TypeExpr::Named(name))
            }
        }
    }

    // ---- statements --------------------------------------------------

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            let start = self.peek_span();
            let node = self.parse_stmt()?;
            stmts.push(Spanned {
                node,
                span: start.to(self.prev_span),
            });
        }
        let close = self.expect(TokenKind::RBrace)?.span;
        Ok(Block { stmts, close })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek() {
            TokenKind::KwLet => self.parse_let_stmt(),
            TokenKind::KwBuffer => self.parse_buffer_stmt(),
            TokenKind::KwMove => self.parse_move_stmt(),
            TokenKind::KwSync => self.parse_sync_stmt(),
            TokenKind::KwRelease => self.parse_release_stmt(),
            TokenKind::KwDestroy => self.parse_destroy_stmt(),
            TokenKind::KwSwap => self.parse_swap_stmt(),
            TokenKind::KwRotate => self.parse_rotate_stmt(),
            TokenKind::KwSplit => self.parse_split_stmt(),
            TokenKind::KwJoin => self.parse_join_stmt(),
            TokenKind::KwReturn => self.parse_return_stmt(),
            TokenKind::KwIf => self.parse_if_stmt(),
            TokenKind::KwWhile => self.parse_while_stmt(),
            // v2.2: `signal E;`, `wait E;` and `critical { .. }`, keywords
            // only in this shape -- `wait(x);` is still a call.
            TokenKind::Ident(w)
                if (w == "signal" || w == "wait")
                    && matches!(self.peek_at(1), TokenKind::Ident(_))
                    && matches!(self.peek_at(2), TokenKind::Semi) =>
            {
                let is_signal = w == "signal";
                self.bump();
                let name = self.expect_ident()?;
                self.expect(TokenKind::Semi)?;
                Ok(if is_signal {
                    Stmt::Signal { name }
                } else {
                    Stmt::Wait {
                        name,
                        bufs: Vec::new(),
                    }
                })
            }
            // v2.3: `wait E for b;` -- the event hands `b` back to the CPU.
            TokenKind::Ident(w)
                if w == "wait"
                    && matches!(self.peek_at(1), TokenKind::Ident(_))
                    && matches!(self.peek_at(2), TokenKind::Ident(f) if f == "for") =>
            {
                self.bump();
                let name = self.expect_ident()?;
                self.bump(); // `for`
                let mut bufs = vec![self.expect_ident()?];
                while self.at(&TokenKind::Comma) {
                    self.bump();
                    bufs.push(self.expect_ident()?);
                }
                self.expect(TokenKind::Semi)?;
                Ok(Stmt::Wait { name, bufs })
            }
            TokenKind::Ident(w)
                if w == "critical" && matches!(self.peek_at(1), TokenKind::LBrace) =>
            {
                self.bump();
                Ok(Stmt::Critical {
                    body: self.parse_block()?,
                })
            }
            // `name = expr;` (M9) -- look ahead one token so a bare call
            // statement (`tick();`) or other expression statement isn't
            // mistaken for an assignment.
            TokenKind::Ident(_) if matches!(self.peek_at(1), TokenKind::Eq) => {
                self.parse_assign_stmt()
            }
            _ => self.parse_expr_stmt(),
        }
    }

    /// `name = expr;` (M9) -- reassigns an already-declared scalar local.
    /// Deliberately just `Ident = Expr`, not general lvalues (no field or
    /// index targets) -- see `phase_specification.md` §9,
    /// Milestone M9.
    fn parse_assign_stmt(&mut self) -> Result<Stmt, ParseError> {
        let name = self.expect_ident()?;
        self.expect(TokenKind::Eq)?;
        let value = self.parse_expr()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Assign { name, value })
    }

    /// `if cond { .. } else if cond2 { .. } else { .. }` — the `else if`
    /// chain desugars to nested `If`s, each the sole statement of an
    /// implicit else-block, so the AST only ever needs one `If` shape.
    fn parse_if_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwIf)?;
        let cond = self.parse_condition()?;
        let then_block = self.parse_block()?;
        let else_block = if self.at(&TokenKind::KwElse) {
            self.bump();
            if self.at(&TokenKind::KwIf) {
                let start = self.peek_span();
                let inner = self.parse_if_stmt()?;
                let span = start.to(self.prev_span);
                Some(Block {
                    stmts: vec![Spanned { node: inner, span }],
                    close: self.prev_span,
                })
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        Ok(Stmt::If {
            cond,
            then_block,
            else_block,
        })
    }

    fn parse_while_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwWhile)?;
        let cond = self.parse_condition()?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_let_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwLet)?;
        let name = self.expect_ident()?;
        let mut ty = None;
        let mut domain = None;
        if self.at(&TokenKind::Colon) {
            self.bump();
            ty = Some(self.parse_type_expr()?);
            domain = self.opt_domain()?;
        }
        let init = if self.at(&TokenKind::Eq) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::VarDecl {
            name,
            ty,
            domain,
            init,
        })
    }

    /// `buffer<Sample, 1024> radio_buf @DMA;`
    fn parse_buffer_stmt(&mut self) -> Result<Stmt, ParseError> {
        let ty = self.parse_type_expr()?; // consumes the `buffer<...>` form
        let name = self.expect_ident()?;
        let domain = self.opt_domain()?;
        let init = if self.at(&TokenKind::Eq) {
            self.bump();
            Some(self.parse_expr()?)
        } else {
            None
        };
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::VarDecl {
            name,
            ty: Some(ty),
            domain,
            init,
        })
    }

    fn parse_move_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwMove)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::Arrow)?;
        let to = self.parse_domain()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Move { name, to })
    }

    fn parse_sync_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwSync)?;
        self.expect(TokenKind::LParen)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::RParen)?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Sync { name })
    }

    fn parse_release_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwRelease)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Release { name })
    }

    /// `a, b, c` -- one or more comma-separated names.
    fn parse_name_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut names = vec![self.expect_ident()?];
        while self.at(&TokenKind::Comma) {
            self.bump();
            names.push(self.expect_ident()?);
        }
        Ok(names)
    }

    /// `rotate a, b, c;`
    fn parse_rotate_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwRotate)?;
        let names = self.parse_name_list()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Rotate { names })
    }

    /// `split buf into lo, hi;`
    fn parse_split_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwSplit)?;
        let parent = self.expect_ident()?;
        self.expect(TokenKind::KwInto)?;
        let parts = self.parse_name_list()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Split { parent, parts })
    }

    /// `join lo, hi into buf;`
    fn parse_join_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwJoin)?;
        let parts = self.parse_name_list()?;
        self.expect(TokenKind::KwInto)?;
        let parent = self.expect_ident()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Join { parent, parts })
    }

    /// `swap a, b;`
    fn parse_swap_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwSwap)?;
        let a = self.expect_ident()?;
        self.expect(TokenKind::Comma)?;
        let b = self.expect_ident()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Swap { a, b })
    }

    fn parse_destroy_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwDestroy)?;
        let name = self.expect_ident()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Destroy { name })
    }

    fn parse_return_stmt(&mut self) -> Result<Stmt, ParseError> {
        self.expect(TokenKind::KwReturn)?;
        let value = if self.at(&TokenKind::Semi) {
            None
        } else {
            Some(self.parse_expr()?)
        };
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Return(value))
    }

    fn parse_expr_stmt(&mut self) -> Result<Stmt, ParseError> {
        let e = self.parse_expr()?;
        self.expect(TokenKind::Semi)?;
        Ok(Stmt::Expr(e))
    }

    // ---- expressions ---------------------------------------------------
    // Precedence climbing, loosest first (as in Rust, so `x & 1 == 1` is
    // `(x & 1) == 1`): comparisons, `|`, `^`, `&`, `<< >>`, `+ -`, `* /`,
    // then unary `~`.

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_comparison()
    }

    /// Parse an `if`/`while` condition: same expression grammar, but a bare
    /// `Ident {` must not be swallowed as a struct literal, since `{` opens
    /// the following block instead. See `restrict_struct_lit`.
    fn parse_condition(&mut self) -> Result<Expr, ParseError> {
        let prev = self.restrict_struct_lit;
        self.restrict_struct_lit = true;
        let result = self.parse_expr();
        self.restrict_struct_lit = prev;
        result
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_bit_or()?;
        loop {
            let op = match self.peek() {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::NotEq => BinOp::NotEq,
                TokenKind::LAngle => BinOp::Lt,
                TokenKind::RAngle => BinOp::Gt,
                TokenKind::LtEq => BinOp::LtEq,
                TokenKind::GtEq => BinOp::GtEq,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_bit_or()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    /// One left-associative level of binary operators: `ops` maps a token
    /// to its operator, `next` parses the operands (the next tighter level).
    fn parse_level(
        &mut self,
        ops: &[(TokenKind, BinOp)],
        next: fn(&mut Self) -> Result<Expr, ParseError>,
    ) -> Result<Expr, ParseError> {
        let mut lhs = next(self)?;
        while let Some(op) = ops.iter().find(|(t, _)| self.at(t)).map(|(_, op)| *op) {
            self.bump();
            let rhs = next(self)?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_bit_or(&mut self) -> Result<Expr, ParseError> {
        self.parse_level(&[(TokenKind::Pipe, BinOp::BitOr)], Self::parse_bit_xor)
    }

    fn parse_bit_xor(&mut self) -> Result<Expr, ParseError> {
        self.parse_level(&[(TokenKind::Caret, BinOp::BitXor)], Self::parse_bit_and)
    }

    fn parse_bit_and(&mut self) -> Result<Expr, ParseError> {
        self.parse_level(&[(TokenKind::Amp, BinOp::BitAnd)], Self::parse_shift)
    }

    fn parse_shift(&mut self) -> Result<Expr, ParseError> {
        self.parse_level(
            &[(TokenKind::Shl, BinOp::Shl), (TokenKind::Shr, BinOp::Shr)],
            Self::parse_additive,
        )
    }

    fn parse_additive(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            self.bump();
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        // v2.4: `~x`. (No unary `-`: values that matter here are unsigned.)
        if self.at(&TokenKind::Tilde) {
            self.bump();
            return Ok(Expr::BitNot(Box::new(self.parse_unary()?)));
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.parse_primary()?;
        loop {
            if self.at(&TokenKind::Dot) {
                self.bump();
                let field = self.expect_ident()?;
                e = Expr::FieldAccess {
                    base: Box::new(e),
                    field,
                };
            } else if self.at(&TokenKind::LBracket) {
                // v2.4: `REG[i]`.
                self.bump();
                let index = self.parse_expr()?;
                self.expect(TokenKind::RBracket)?;
                e = Expr::Index {
                    base: Box::new(e),
                    index: Box::new(index),
                };
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let span = self.peek_span();
        match self.peek().clone() {
            TokenKind::Int(n) => {
                self.bump();
                Ok(Expr::IntLit(n))
            }
            TokenKind::Float(n) => {
                self.bump();
                Ok(Expr::FloatLit(n))
            }
            TokenKind::Str(s) => {
                self.bump();
                Ok(Expr::StringLit(s))
            }
            TokenKind::KwTrue => {
                self.bump();
                Ok(Expr::BoolLit(true))
            }
            TokenKind::KwFalse => {
                self.bump();
                Ok(Expr::BoolLit(false))
            }
            TokenKind::KwBorrow => {
                self.bump();
                let target = self.expect_ident()?;
                let mode = match self.peek() {
                    TokenKind::KwRead => BorrowMode::Read,
                    TokenKind::KwWrite => BorrowMode::Write,
                    other => {
                        return Err(ParseError {
                            message: format!(
                                "expected 'read' or 'write' after borrow target, found {:?}",
                                other
                            ),
                            span: self.peek_span(),
                        })
                    }
                };
                self.bump();
                Ok(Expr::Borrow { target, mode })
            }
            TokenKind::LParen => {
                self.bump();
                let e = self.parse_expr()?;
                self.expect(TokenKind::RParen)?;
                Ok(e)
            }
            TokenKind::Ident(name) => {
                self.bump();
                self.parse_ident_led(name, span)
            }
            other => Err(ParseError {
                message: format!("unexpected token {:?} in expression", other),
                span,
            }),
        }
    }

    /// Continue parsing after consuming a leading identifier: could be a
    /// bare ident, a call `f(...)`, a struct literal `Name { ... }`, or a
    /// domain reference `NAME @DOMAIN`.
    fn parse_ident_led(&mut self, name: String, span: Span) -> Result<Expr, ParseError> {
        match self.peek() {
            TokenKind::LParen => {
                self.bump();
                let mut args = Vec::new();
                while !self.at(&TokenKind::RParen) {
                    args.push(self.parse_expr()?);
                    if self.at(&TokenKind::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.expect(TokenKind::RParen)?;
                Ok(Expr::Call { callee: name, args })
            }
            TokenKind::LBrace if !self.restrict_struct_lit => {
                self.bump();
                let mut fields = Vec::new();
                while !self.at(&TokenKind::RBrace) {
                    let fname = self.expect_ident()?;
                    self.expect(TokenKind::Colon)?;
                    let fval = self.parse_expr()?;
                    fields.push((fname, fval));
                    if self.at(&TokenKind::Comma) {
                        self.bump();
                    } else {
                        break;
                    }
                }
                self.expect(TokenKind::RBrace)?;
                Ok(Expr::StructLit { name, fields })
            }
            TokenKind::At => {
                let domain = self.parse_domain()?;
                Ok(Expr::DomainRef { name, domain })
            }
            _ => {
                let _ = span;
                Ok(Expr::Ident(name))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_split_and_join_are_parsed() {
        let p = parse("fn main() { rotate a, b, c; split buf into lo, hi; join lo, hi into buf; }")
            .unwrap();
        let Item::Fn(f) = &p.items[0] else {
            panic!("expected fn")
        };
        let v = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            f.body.stmts[0].node,
            Stmt::Rotate {
                names: v(&["a", "b", "c"])
            }
        );
        assert_eq!(
            f.body.stmts[1].node,
            Stmt::Split {
                parent: "buf".into(),
                parts: v(&["lo", "hi"])
            }
        );
        assert_eq!(
            f.body.stmts[2].node,
            Stmt::Join {
                parent: "buf".into(),
                parts: v(&["lo", "hi"])
            }
        );
    }

    #[test]
    fn swap_statement_is_parsed() {
        let p = parse("fn main() { swap ping, pong; }").unwrap();
        let Item::Fn(f) = &p.items[0] else {
            panic!("expected fn")
        };
        assert_eq!(
            f.body.stmts[0].node,
            Stmt::Swap {
                a: "ping".into(),
                b: "pong".into()
            }
        );
    }

    #[test]
    fn parameter_access_modes_are_parsed() {
        let p = parse(
            "extern fn f(a: read buffer<u8, 4> @RAM, b: write buffer<u8, 4> @RAM, c: buffer<u8, 4> @RAM); fn main() {}",
        )
        .unwrap();
        let Item::ExternFn(f) = &p.items[0] else {
            panic!("expected extern fn")
        };
        let modes: Vec<_> = f.params.iter().map(|p| p.access).collect();
        assert_eq!(
            modes,
            vec![Some(BorrowMode::Read), Some(BorrowMode::Write), None]
        );
    }

    #[test]
    fn parameter_domain_change_is_parsed() {
        let p = parse("extern fn start(dst: buffer<u8, 4> @RAM -> @DMA); fn main() {}").unwrap();
        let Item::ExternFn(f) = &p.items[0] else {
            panic!("expected extern fn")
        };
        assert_eq!(f.params[0].domain, Some(Domain::Ram));
        assert_eq!(f.params[0].domain_after, Some(Domain::Dma));
    }

    #[test]
    fn return_type_arrow_is_not_a_domain_change() {
        let p = parse("state P { A } extern fn f(x: buffer<u8, 4> @RAM) -> P<A>; fn main() {}")
            .unwrap();
        let Item::ExternFn(f) = &p.items[1] else {
            panic!("expected extern fn")
        };
        assert_eq!(f.params[0].domain_after, None);
        assert!(f.return_type.is_some());
    }

    #[test]
    fn domain_change_without_a_starting_domain_is_an_error() {
        let err = parse("extern fn f(x: buffer<u8, 4> -> @DMA); fn main() {}").unwrap_err();
        assert!(
            err.message.contains("has `->` but no starting domain"),
            "{}",
            err.message
        );
    }

    #[test]
    fn parses_empty_entity() {
        let prog = parse("entity Sample : @RAM { value: f32 }").unwrap();
        assert_eq!(
            prog.items[0],
            Item::Entity(EntityDecl {
                name: "Sample".into(),
                domain: Some(Domain::Ram),
                fields: vec![FieldDecl {
                    name: "value".into(),
                    ty: TypeExpr::Named("f32".into())
                }],
            })
        );
    }

    #[test]
    fn parses_entity_without_domain() {
        let prog = parse("entity Foo { x: i32 }").unwrap();
        match &prog.items[0] {
            Item::Entity(e) => assert_eq!(e.domain, None),
            _ => panic!("expected entity"),
        }
    }

    #[test]
    fn parses_state_decl() {
        let prog = parse("state Packet { Received -> Decoded -> Validated }").unwrap();
        assert_eq!(
            prog.items[0],
            Item::State(StateDecl {
                name: "Packet".into(),
                states: vec!["Received".into(), "Decoded".into(), "Validated".into()],
            })
        );
    }

    #[test]
    fn parses_extern_fn_decl() {
        let src = "extern fn fir_filter(input: buffer<Sample,1024> @RAM, output: buffer<Sample,1024> @RAM);";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::ExternFn(f) => {
                assert_eq!(f.name, "fir_filter");
                assert_eq!(f.params.len(), 2);
                assert_eq!(f.params[0].domain, Some(Domain::Ram));
                assert_eq!(
                    f.params[0].ty,
                    TypeExpr::Buffer {
                        elem: Box::new(TypeExpr::Named("Sample".into())),
                        len: BufferLen::Fixed(1024)
                    }
                );
            }
            other => panic!("expected extern fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_let_with_struct_lit_init() {
        let prog = parse("fn main() { let s: Sample @RAM = Sample { value: 0.0 }; }").unwrap();
        match &prog.items[0] {
            Item::Fn(f) => {
                assert_eq!(f.name, "main");
                assert_eq!(
                    f.body.stmts[0].node,
                    Stmt::VarDecl {
                        name: "s".into(),
                        ty: Some(TypeExpr::Named("Sample".into())),
                        domain: Some(Domain::Ram),
                        init: Some(Expr::StructLit {
                            name: "Sample".into(),
                            fields: vec![("value".into(), Expr::FloatLit(0.0))],
                        }),
                    }
                );
            }
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_buffer_decl_statement() {
        let prog = parse("fn main() { buffer<Sample, 1024> raw @DMA; }").unwrap();
        match &prog.items[0] {
            Item::Fn(f) => assert_eq!(
                f.body.stmts[0].node,
                Stmt::VarDecl {
                    name: "raw".into(),
                    ty: Some(TypeExpr::Buffer {
                        elem: Box::new(TypeExpr::Named("Sample".into())),
                        len: BufferLen::Fixed(1024)
                    }),
                    domain: Some(Domain::Dma),
                    init: None,
                }
            ),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_move_sync_borrow_release() {
        let src = r#"
            fn main() {
                move raw -> @DMA;
                sync(raw);
                let view = borrow raw read;
                release view;
            }
        "#;
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => {
                assert_eq!(
                    f.body.stmts[0].node,
                    Stmt::Move {
                        name: "raw".into(),
                        to: Domain::Dma
                    }
                );
                assert_eq!(f.body.stmts[1].node, Stmt::Sync { name: "raw".into() });
                assert_eq!(
                    f.body.stmts[2].node,
                    Stmt::VarDecl {
                        name: "view".into(),
                        ty: None,
                        domain: None,
                        init: Some(Expr::Borrow {
                            target: "raw".into(),
                            mode: BorrowMode::Read
                        }),
                    }
                );
                assert_eq!(
                    f.body.stmts[3].node,
                    Stmt::Release {
                        name: "view".into()
                    }
                );
            }
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_volatile_mmio_calls() {
        let src = r#"
            fn main() {
                let status: u32 = volatile_read(UART_STATUS @MMIO);
                volatile_write(UART_CONTROL @MMIO, 1);
            }
        "#;
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => {
                assert_eq!(
                    f.body.stmts[0].node,
                    Stmt::VarDecl {
                        name: "status".into(),
                        ty: Some(TypeExpr::Named("u32".into())),
                        domain: None,
                        init: Some(Expr::Call {
                            callee: "volatile_read".into(),
                            args: vec![Expr::DomainRef {
                                name: "UART_STATUS".into(),
                                domain: Domain::Mmio
                            }],
                        }),
                    }
                );
                assert_eq!(
                    f.body.stmts[1].node,
                    Stmt::Expr(Expr::Call {
                        callee: "volatile_write".into(),
                        args: vec![
                            Expr::DomainRef {
                                name: "UART_CONTROL".into(),
                                domain: Domain::Mmio
                            },
                            Expr::IntLit(1)
                        ],
                    })
                );
            }
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_full_radio_pipeline_example() {
        let src = r#"
            extern fn fir_filter(in_: buffer<Sample, 1024> @RAM,
                                  out: buffer<Sample, 1024> @RAM);

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
        let prog = parse(src).unwrap();
        assert_eq!(prog.items.len(), 2);
        match &prog.items[1] {
            Item::Fn(f) => assert_eq!(f.body.stmts.len(), 8),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_binary_expressions_with_precedence() {
        let prog = parse("fn main() { let x = 1 + 2 * 3; }").unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::VarDecl { init: Some(e), .. } => {
                    assert_eq!(
                        *e,
                        Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(Expr::IntLit(1)),
                            rhs: Box::new(Expr::Binary {
                                op: BinOp::Mul,
                                lhs: Box::new(Expr::IntLit(2)),
                                rhs: Box::new(Expr::IntLit(3)),
                            }),
                        }
                    );
                }
                other => panic!("expected var decl with init, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_stateful_type_in_fn_signature() {
        let src = "fn decode(p: Packet<Received>) -> Packet<Decoded> { }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => {
                assert_eq!(
                    f.params[0].ty,
                    TypeExpr::Stateful {
                        name: "Packet".into(),
                        state: "Received".into()
                    }
                );
                assert_eq!(
                    f.return_type,
                    Some(TypeExpr::Stateful {
                        name: "Packet".into(),
                        state: "Decoded".into()
                    })
                );
            }
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_stateful_type_in_let_annotation() {
        let src = "fn main() { let p: Packet<Received> = receive(); }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::VarDecl { ty, .. } => assert_eq!(
                    *ty,
                    Some(TypeExpr::Stateful {
                        name: "Packet".into(),
                        state: "Received".into()
                    })
                ),
                other => panic!("expected var decl, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_if_without_else() {
        let src = "fn main() { if flag { destroy x; } }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::If {
                    cond,
                    then_block,
                    else_block,
                } => {
                    assert_eq!(*cond, Expr::Ident("flag".into()));
                    assert_eq!(then_block.stmts.len(), 1);
                    assert_eq!(*else_block, None);
                }
                other => panic!("expected if, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_if_else() {
        let src = "fn main() { if flag { destroy x; } else { destroy y; } }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::If { else_block, .. } => {
                    let eb = else_block.as_ref().expect("expected else block");
                    assert_eq!(eb.stmts.len(), 1);
                }
                other => panic!("expected if, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_else_if_chain_as_nested_if() {
        let src = "fn main() { if a { destroy x; } else if b { destroy y; } else { destroy z; } }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::If { else_block, .. } => {
                    let eb = else_block.as_ref().unwrap();
                    assert_eq!(eb.stmts.len(), 1);
                    match &eb.stmts[0].node {
                        Stmt::If {
                            cond, else_block, ..
                        } => {
                            assert_eq!(*cond, Expr::Ident("b".into()));
                            assert!(else_block.is_some());
                        }
                        other => panic!("expected nested if, got {:?}", other),
                    }
                }
                other => panic!("expected if, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_while() {
        let src = "fn main() { while flag { destroy x; } }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::While { cond, body } => {
                    assert_eq!(*cond, Expr::Ident("flag".into()));
                    assert_eq!(body.stmts.len(), 1);
                }
                other => panic!("expected while, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    // ---- M9: assignment -------------------------------------------------

    #[test]
    fn parses_assignment_statement() {
        let src = "fn main() { n = n + 1; }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::Assign { name, value } => {
                    assert_eq!(name, "n");
                    assert_eq!(
                        *value,
                        Expr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(Expr::Ident("n".into())),
                            rhs: Box::new(Expr::IntLit(1)),
                        }
                    );
                }
                other => panic!("expected assign, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn assignment_inside_while_body_parses() {
        let src = "fn main() { while n < 5 { n = n + 1; } }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::While { body, .. } => {
                    assert_eq!(body.stmts.len(), 1);
                    assert!(matches!(body.stmts[0].node, Stmt::Assign { .. }));
                }
                other => panic!("expected while, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn bare_call_statement_is_not_mistaken_for_an_assignment() {
        // `tick();` starts with an Ident but the next token is `(`, not
        // `=` -- must still parse as a plain expression statement.
        let src = "extern fn tick(); fn main() { tick(); }";
        let prog = parse(src).unwrap();
        match &prog.items[1] {
            Item::Fn(f) => assert!(matches!(
                f.body.stmts[0].node,
                Stmt::Expr(Expr::Call { .. })
            )),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn equality_comparison_is_not_mistaken_for_an_assignment() {
        // `n == 0` inside a condition must still tokenize/parse as EqEq,
        // not trip the `Ident` + `Eq` assignment lookahead.
        let src = "fn main() { if n == 0 { tick(); } }";
        assert!(parse(src).is_ok());
    }

    #[test]
    fn bare_ident_condition_is_not_parsed_as_struct_literal() {
        // Without the restrict_struct_lit fix, `flag { ... }` would try to
        // parse as a struct literal and fail on the first real statement.
        let src = "fn main() { if flag { let x = 1; } }";
        assert!(parse(src).is_ok());
    }

    #[test]
    fn struct_literal_still_works_outside_condition_position() {
        let src = "fn main() { let s = Sample { value: 1.0 }; }";
        let prog = parse(src).unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0].node {
                Stmt::VarDecl {
                    init: Some(Expr::StructLit { name, .. }),
                    ..
                } => assert_eq!(name, "Sample"),
                other => panic!("expected struct-lit var decl, got {:?}", other),
            },
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn missing_semicolon_is_a_parse_error() {
        let err = parse("fn main() { let x = 1 }").unwrap_err();
        assert!(err.message.contains("Semi") || err.message.contains(";"));
    }

    #[test]
    fn unknown_domain_is_a_parse_error() {
        let err = parse("fn main() { move x -> @NOT_A_DOMAIN; }").unwrap_err();
        assert!(err.message.contains("unknown domain"));
    }

    #[test]
    fn device_register_maps_are_parsed() {
        let p = parse(
            "device Uart0 @ 0x40004000 {
                DATA: read write u32 @ 0x00;
                STATE: read u32 @ 0x04 { TXFULL: bit 0, RXFULL: bit 1 }
                CTRL: write read u32 @ 0x08 = 0x3 { BAUD: bits 4..=11 }
            }
            fn main() { let device: u32 = 0; }",
        )
        .unwrap();
        let Item::Device(d) = &p.items[0] else {
            panic!()
        };
        assert_eq!(
            (d.name.as_str(), d.base, d.regs.len()),
            ("Uart0", 0x4000_4000, 3)
        );
        let st = d.reg("STATE").unwrap();
        assert!(st.readable && !st.writable);
        assert_eq!((st.offset, st.field("RXFULL").unwrap().lo), (4, 1));
        let ctrl = d.reg("CTRL").unwrap();
        assert!(ctrl.readable && ctrl.writable);
        assert_eq!(ctrl.reset, Some(3));
        let baud = ctrl.field("BAUD").unwrap();
        assert_eq!((baud.lo, baud.hi), (4, 11));
    }

    #[test]
    fn a_register_without_an_access_mode_is_an_error() {
        let e = parse("device D @ 0 { R: u32 @ 0; }").unwrap_err();
        assert!(e.message.contains("needs an access mode"), "{}", e.message);
    }

    #[test]
    fn device_types_and_instances_are_parsed() {
        let p = parse(
            "device type Uart { DATA: read write u32 @ 0; }
             device Uart0: Uart @ 0x40004000;
             device type @ 0x1000 { R: read u8 @ 0; }",
        )
        .unwrap();
        let Item::DeviceType(t) = &p.items[0] else {
            panic!()
        };
        assert_eq!((t.name.as_str(), t.regs.len()), ("Uart", 1));
        let Item::Device(d) = &p.items[1] else {
            panic!()
        };
        assert_eq!(d.of_type.as_ref().map(|(t, _)| t.as_str()), Some("Uart"));
        assert_eq!((d.base, d.regs.len()), (0x4000_4000, 0));
        assert_eq!(device_regs(&p, d).map(|r| r.len()), Some(1));
        // A device may still be named `type`.
        let Item::Device(odd) = &p.items[2] else {
            panic!()
        };
        assert_eq!(odd.name, "type");
    }

    #[test]
    fn register_arrays_field_kinds_and_indexes_are_parsed() {
        let p = parse(
            "device D @ 0x40000000 {
                 CR[8]: read write u32 @ 0x10 step 0x18 { EN: bit 0, TCIF: bit 5 w1c, UIF: bit 6 rc_w0, ERR: bit 7 rc }
                 B[2]: read u8 @ 0x0;
             }
             fn main() { volatile_write(D.CR[3].EN, 1); }",
        )
        .unwrap();
        let Item::Device(d) = &p.items[0] else {
            panic!()
        };
        assert_eq!(d.regs[0].array, Some((8, 0x18)));
        assert_eq!(d.regs[1].array, Some((2, 1))); // default step: its size
        let kinds: Vec<FieldKind> = d.regs[0].fields.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FieldKind::Plain,
                FieldKind::W1c,
                FieldKind::RcW0,
                FieldKind::Rc
            ]
        );
        let Item::Fn(f) = &p.items[1] else { panic!() };
        let Stmt::Expr(Expr::Call { args, .. }) = &f.body.stmts[0].node else {
            panic!()
        };
        let path = reg_path(&args[0]).unwrap();
        assert_eq!((path.device, path.reg, path.field), ("D", "CR", Some("EN")));
        assert_eq!(path.index, Some(&Expr::IntLit(3)));
        assert!(parse("device D @ 0 { A: read u32 @ 0 step 4; }").is_err());
    }

    #[test]
    fn bit_operations_bind_like_rust_and_consts_are_items() {
        let p = parse(
            "const M: u32 = 1 << 4;
             fn main() { x = a | b ^ c & d << 1 + 2 == ~e; }",
        )
        .unwrap();
        let Item::Const(c) = &p.items[0] else {
            panic!()
        };
        assert_eq!((c.name.as_str(), c.ty.as_str()), ("M", "u32"));
        let Item::Fn(f) = &p.items[1] else { panic!() };
        let Stmt::Assign { value, .. } = &f.body.stmts[0].node else {
            panic!()
        };
        fn show(e: &Expr) -> String {
            match e {
                Expr::Ident(n) => n.clone(),
                Expr::IntLit(v) => v.to_string(),
                Expr::BitNot(x) => format!("~{}", show(x)),
                Expr::Binary { op, lhs, rhs } => {
                    format!("({} {} {})", show(lhs), op.symbol(), show(rhs))
                }
                _ => "?".into(),
            }
        }
        assert_eq!(show(value), "((a | (b ^ (c & (d << (1 + 2))))) == ~e)");
        // `const` is still an ordinary name elsewhere.
        parse("fn const() {} fn main() { const(); }").unwrap();
    }

    #[test]
    fn wait_for_a_buffer_is_parsed() {
        let p = parse("event ht; fn main() { wait ht for lo; wait ht; }").unwrap();
        let Item::Fn(m) = &p.items[1] else { panic!() };
        assert!(matches!(&m.body.stmts[0].node,
            Stmt::Wait { name, bufs } if name == "ht" && bufs == &["lo"]));
        assert!(matches!(
            &m.body.stmts[1].node,
            Stmt::Wait { bufs, .. } if bufs.is_empty()
        ));
    }

    #[test]
    fn interrupts_events_and_critical_sections_are_parsed() {
        let p = parse(
            "event tick;
             interrupt fn on_timer() @ irq 8 { signal tick; }
             fn wait(x: u32) { }
             fn main() { wait tick; critical { wait(1); } }",
        )
        .unwrap();
        let Item::Event(e) = &p.items[0] else {
            panic!()
        };
        assert_eq!(e.name, "tick");
        let Item::Fn(h) = &p.items[1] else { panic!() };
        assert_eq!(h.irq.map(|(n, _)| n), Some(8));
        // v3.0: or by a constant, resolved once the program is parsed.
        let q = parse("const TIM2_IRQn: u32 = 4 * 7; interrupt fn t() @ irq TIM2_IRQn { }").unwrap();
        let Item::Fn(t) = &q.items[1] else { panic!() };
        assert_eq!(t.irq.map(|(n, _)| n), Some(28));
        assert_eq!(t.irq_name.as_deref(), Some("TIM2_IRQn"));
        assert!(matches!(&h.body.stmts[0].node, Stmt::Signal { name } if name == "tick"));
        let Item::Fn(m) = &p.items[3] else { panic!() };
        assert!(
            matches!(&m.body.stmts[0].node, Stmt::Wait { name, bufs } if name == "tick" && bufs.is_empty())
        );
        let Stmt::Critical { body } = &m.body.stmts[1].node else {
            panic!()
        };
        // `wait(1)` is still a call.
        assert!(
            matches!(&body.stmts[0].node, Stmt::Expr(Expr::Call { callee, .. }) if callee == "wait")
        );
        assert!(parse("interrupt fn h() { }")
            .unwrap_err()
            .message
            .contains("@ irq N"));
    }
}
