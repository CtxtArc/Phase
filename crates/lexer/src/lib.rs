//! Hand-written lexer for PHASE.
//!
//! Turns source text into a flat `Vec<Token>` terminated by `Eof`. No
//! external crates: this is intentionally simple (single pass over chars)
//! since the token set is small.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals & identifiers
    Ident(String),
    Int(i64),
    Float(f64),
    Str(String),

    // Keywords
    KwEntity,
    KwFn,
    KwLet,
    KwMove,
    KwSync,
    KwBorrow,
    KwRelease,
    KwDestroy,
    KwState,
    KwIf,
    KwElse,
    KwWhile,
    KwReturn,
    KwExtern,
    KwRead,
    KwWrite,
    KwBuffer,
    KwTrue,
    KwFalse,

    // Punctuation
    LBrace,   // {
    RBrace,   // }
    LParen,   // (
    RParen,   // )
    LAngle,   // <
    RAngle,   // >
    Colon,    // :
    Semi,     // ;
    Comma,    // ,
    Dot,      // .
    Arrow,    // ->
    At,       // @
    Eq,       // =
    EqEq,     // ==
    NotEq,    // !=
    LtEq,     // <=
    GtEq,     // >=
    Plus,     // +
    Minus,    // -
    Star,     // *
    Slash,    // /
    Bang,     // !

    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lex error at {}:{}: {}",
            self.span.line, self.span.col, self.message
        )
    }
}

fn keyword(word: &str) -> Option<TokenKind> {
    use TokenKind::*;
    Some(match word {
        "entity" => KwEntity,
        "fn" => KwFn,
        "let" => KwLet,
        "move" => KwMove,
        "sync" => KwSync,
        "borrow" => KwBorrow,
        "release" => KwRelease,
        "destroy" => KwDestroy,
        "state" => KwState,
        "if" => KwIf,
        "else" => KwElse,
        "while" => KwWhile,
        "return" => KwReturn,
        "extern" => KwExtern,
        "read" => KwRead,
        "write" => KwWrite,
        "buffer" => KwBuffer,
        "true" => KwTrue,
        "false" => KwFalse,
        _ => return None,
    })
}

pub struct Lexer<'a> {
    chars: Vec<char>,
    pos: usize,
    line: u32,
    col: u32,
    _src: &'a str,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            chars: src.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
            _src: src,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek2(&self) -> Option<char> {
        self.chars.get(self.pos + 1).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn span(&self) -> Span {
        Span {
            line: self.line,
            col: self.col,
        }
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.bump();
                }
                Some('/') if self.peek2() == Some('/') => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek2() == Some('*') => {
                    self.bump();
                    self.bump();
                    loop {
                        match self.peek() {
                            None => break,
                            Some('*') if self.peek2() == Some('/') => {
                                self.bump();
                                self.bump();
                                break;
                            }
                            _ => {
                                self.bump();
                            }
                        }
                    }
                }
                _ => break,
            }
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace_and_comments();
            let start = self.span();
            let Some(c) = self.peek() else {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: start,
                });
                break;
            };

            let kind = if c.is_ascii_alphabetic() || c == '_' {
                self.lex_ident_or_keyword()
            } else if c.is_ascii_digit() {
                self.lex_number(start)?
            } else if c == '"' {
                self.lex_string(start)?
            } else {
                self.lex_punct(start)?
            };

            tokens.push(Token { kind, span: start });
        }
        Ok(tokens)
    }

    fn lex_ident_or_keyword(&mut self) -> TokenKind {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        keyword(&s).unwrap_or(TokenKind::Ident(s))
    }

    fn lex_number(&mut self, start: Span) -> Result<TokenKind, LexError> {
        let mut s = String::new();
        let mut is_float = false;
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.bump();
            } else if c == '.' && !is_float && matches!(self.peek2(), Some(d) if d.is_ascii_digit())
            {
                is_float = true;
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if is_float {
            s.parse::<f64>()
                .map(TokenKind::Float)
                .map_err(|e| LexError {
                    message: format!("invalid float literal '{s}': {e}"),
                    span: start,
                })
        } else {
            s.parse::<i64>()
                .map(TokenKind::Int)
                .map_err(|e| LexError {
                    message: format!("invalid integer literal '{s}': {e}"),
                    span: start,
                })
        }
    }

    fn lex_string(&mut self, start: Span) -> Result<TokenKind, LexError> {
        self.bump(); // opening quote
        let mut s = String::new();
        loop {
            match self.bump() {
                None => {
                    return Err(LexError {
                        message: "unterminated string literal".into(),
                        span: start,
                    })
                }
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some(other) => s.push(other),
                    None => {
                        return Err(LexError {
                            message: "unterminated escape in string literal".into(),
                            span: start,
                        })
                    }
                },
                Some(c) => s.push(c),
            }
        }
        Ok(TokenKind::Str(s))
    }

    fn lex_punct(&mut self, start: Span) -> Result<TokenKind, LexError> {
        use TokenKind::*;
        let c = self.bump().unwrap();
        let kind = match c {
            '{' => LBrace,
            '}' => RBrace,
            '(' => LParen,
            ')' => RParen,
            '<' => {
                if self.peek() == Some('=') {
                    self.bump();
                    LtEq
                } else {
                    LAngle
                }
            }
            '>' => {
                if self.peek() == Some('=') {
                    self.bump();
                    GtEq
                } else {
                    RAngle
                }
            }
            ':' => Colon,
            ';' => Semi,
            ',' => Comma,
            '.' => Dot,
            '@' => At,
            '=' => {
                if self.peek() == Some('=') {
                    self.bump();
                    EqEq
                } else {
                    Eq
                }
            }
            '!' => {
                if self.peek() == Some('=') {
                    self.bump();
                    NotEq
                } else {
                    Bang
                }
            }
            '+' => Plus,
            '-' => {
                if self.peek() == Some('>') {
                    self.bump();
                    Arrow
                } else {
                    Minus
                }
            }
            '*' => Star,
            '/' => Slash,
            other => {
                return Err(LexError {
                    message: format!("unexpected character '{other}'"),
                    span: start,
                })
            }
        };
        Ok(kind)
    }
}

pub fn tokenize(src: &str) -> Result<Vec<Token>, LexError> {
    Lexer::new(src).tokenize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        tokenize(src)
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn empty_input_is_just_eof() {
        assert_eq!(kinds(""), vec![Eof]);
    }

    #[test]
    fn keywords_are_recognized() {
        assert_eq!(
            kinds("entity fn let move sync borrow release destroy state"),
            vec![
                KwEntity, KwFn, KwLet, KwMove, KwSync, KwBorrow, KwRelease, KwDestroy, KwState,
                Eof
            ]
        );
    }

    #[test]
    fn identifiers_vs_keywords() {
        assert_eq!(
            kinds("radio_buf entity2 _x"),
            vec![
                Ident("radio_buf".into()),
                Ident("entity2".into()),
                Ident("_x".into()),
                Eof
            ]
        );
    }

    #[test]
    fn integer_and_float_literals() {
        assert_eq!(
            kinds("1024 0.0 3.14 42"),
            vec![Int(1024), Float(0.0), Float(3.14), Int(42), Eof]
        );
    }

    #[test]
    fn string_literal_with_escape() {
        assert_eq!(
            kinds("\"hello\\nworld\""),
            vec![Str("hello\nworld".into()), Eof]
        );
    }

    #[test]
    fn domain_tag_lexes_as_at_then_ident() {
        assert_eq!(kinds("@DMA"), vec![At, Ident("DMA".into()), Eof]);
    }

    #[test]
    fn arrow_vs_minus() {
        assert_eq!(kinds("-> - ->"), vec![Arrow, Minus, Arrow, Eof]);
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(
            kinds("== != <= >= < >"),
            vec![EqEq, NotEq, LtEq, GtEq, LAngle, RAngle, Eof]
        );
    }

    #[test]
    fn line_comment_is_skipped() {
        assert_eq!(kinds("let x // comment\n= 1;"), vec![KwLet, Ident("x".into()), Eq, Int(1), Semi, Eof]);
    }

    #[test]
    fn block_comment_is_skipped() {
        assert_eq!(
            kinds("let /* comment */ x = 1;"),
            vec![KwLet, Ident("x".into()), Eq, Int(1), Semi, Eof]
        );
    }

    #[test]
    fn full_move_statement() {
        assert_eq!(
            kinds("move radio_buf -> @DMA;"),
            vec![
                KwMove,
                Ident("radio_buf".into()),
                Arrow,
                At,
                Ident("DMA".into()),
                Semi,
                Eof
            ]
        );
    }

    #[test]
    fn unterminated_string_is_an_error() {
        let err = tokenize("\"abc").unwrap_err();
        assert!(err.message.contains("unterminated"));
    }

    #[test]
    fn unexpected_character_is_an_error() {
        let err = tokenize("let x = 1 $ 2;").unwrap_err();
        assert!(err.message.contains("unexpected character"));
    }

    #[test]
    fn span_tracks_line_and_col() {
        let toks = tokenize("let\nx").unwrap();
        // 'x' is on line 2, col 1
        assert_eq!(toks[1].span, Span { line: 2, col: 1 });
    }
}
