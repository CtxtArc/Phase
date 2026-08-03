//! Recursive-descent parser for PHASE (v0.1, M1 scope: no `if`/`while` yet).

use phase_ast::*;
use phase_lexer::{tokenize, LexError, Span, Token, TokenKind};

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
}

/// Parse a full PHASE source file into a `Program`.
pub fn parse(src: &str) -> Result<Program, ParseError> {
    let tokens = tokenize(src)?;
    Parser::new(tokens).parse_program()
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    // ---- token stream helpers ----------------------------------------

    fn peek(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn peek_span(&self) -> Span {
        self.tokens[self.pos].span
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    fn bump(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
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
            TokenKind::KwFn => self.parse_fn_decl().map(Item::Fn),
            other => Err(ParseError {
                message: format!(
                    "expected item (entity/state/fn/extern fn), found {:?}",
                    other
                ),
                span: self.peek_span(),
            }),
        }
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
            let name = self.expect_ident()?;
            self.expect(TokenKind::Colon)?;
            let ty = self.parse_type_expr()?;
            let domain = self.opt_domain()?;
            params.push(Param { name, ty, domain });
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
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let return_type = self.parse_return_type()?;
        let body = self.parse_block()?;
        Ok(FnDecl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_extern_fn_decl(&mut self) -> Result<ExternFnDecl, ParseError> {
        self.expect(TokenKind::KwExtern)?;
        self.expect(TokenKind::KwFn)?;
        let name = self.expect_ident()?;
        let params = self.parse_params()?;
        let return_type = self.parse_return_type()?;
        self.expect(TokenKind::Semi)?;
        Ok(ExternFnDecl {
            name,
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
                TokenKind::Int(n) if n >= 0 => n as u64,
                other => {
                    return Err(ParseError {
                        message: format!("expected non-negative integer buffer length, found {:?}", other),
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
            Ok(TypeExpr::Named(name))
        }
    }

    // ---- statements --------------------------------------------------

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        self.expect(TokenKind::LBrace)?;
        let mut stmts = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            stmts.push(self.parse_stmt()?);
        }
        self.expect(TokenKind::RBrace)?;
        Ok(Block { stmts })
    }

    fn parse_stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek() {
            TokenKind::KwLet => self.parse_let_stmt(),
            TokenKind::KwBuffer => self.parse_buffer_stmt(),
            TokenKind::KwMove => self.parse_move_stmt(),
            TokenKind::KwSync => self.parse_sync_stmt(),
            TokenKind::KwRelease => self.parse_release_stmt(),
            TokenKind::KwDestroy => self.parse_destroy_stmt(),
            TokenKind::KwReturn => self.parse_return_stmt(),
            _ => self.parse_expr_stmt(),
        }
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
    // Precedence climbing: comparisons bind loosest, then + -, then * /.

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_comparison()
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_additive()?;
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
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            };
        }
        Ok(lhs)
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
        // v0.1 has no unary operators in the AST yet beyond literals/primary;
        // reserved for future work (e.g. unary `-`). Fall straight through.
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
                            message: format!("expected 'read' or 'write' after borrow target, found {:?}", other),
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
            TokenKind::LBrace => {
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
                        len: 1024
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
                    f.body.stmts[0],
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
                f.body.stmts[0],
                Stmt::VarDecl {
                    name: "raw".into(),
                    ty: Some(TypeExpr::Buffer {
                        elem: Box::new(TypeExpr::Named("Sample".into())),
                        len: 1024
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
                    f.body.stmts[0],
                    Stmt::Move {
                        name: "raw".into(),
                        to: Domain::Dma
                    }
                );
                assert_eq!(
                    f.body.stmts[1],
                    Stmt::Sync {
                        name: "raw".into()
                    }
                );
                assert_eq!(
                    f.body.stmts[2],
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
                    f.body.stmts[3],
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
                    f.body.stmts[0],
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
                    f.body.stmts[1],
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

                move raw -> @RAM;
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
            Item::Fn(f) => assert_eq!(f.body.stmts.len(), 9),
            other => panic!("expected fn, got {:?}", other),
        }
    }

    #[test]
    fn parses_binary_expressions_with_precedence() {
        let prog = parse("fn main() { let x = 1 + 2 * 3; }").unwrap();
        match &prog.items[0] {
            Item::Fn(f) => match &f.body.stmts[0] {
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
    fn missing_semicolon_is_a_parse_error() {
        let err = parse("fn main() { let x = 1 }").unwrap_err();
        assert!(err.message.contains("Semi") || err.message.contains(";"));
    }

    #[test]
    fn unknown_domain_is_a_parse_error() {
        let err = parse("fn main() { move x -> @NOT_A_DOMAIN; }").unwrap_err();
        assert!(err.message.contains("unknown domain"));
    }
}
