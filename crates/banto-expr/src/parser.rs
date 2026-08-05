//! 再帰下降パーサ。優先順位（低い順、C系言語と同じ並び）:
//!
//! ```text
//! expr        := or_expr
//! or_expr     := and_expr ("||" and_expr)*
//! and_expr    := equality ("&&" equality)*
//! equality    := comparison (("==" | "!=") comparison)*
//! comparison  := additive (("<" | ">" | "<=" | ">=") additive)*
//! additive    := multiplicative (("+" | "-") multiplicative)*
//! multiplicative := unary (("*" | "/") unary)*
//! unary       := ("-" | "!") unary | primary
//! primary     := number | "true" | "false" | tag_ref | call | "(" expr ")"
//! tag_ref     := ident "." ident "." ident        (ちょうど3セグメント)
//! call        := ident "(" (expr ("," expr)*)? ")"
//! ```
//!
//! 関数名が既知か（`if`/`min`/`max`/`abs`/`round`/`clamp`/`bit`）・引数の
//! 個数が合っているかは、ここでは検証しない。パーサは構文（かっこの対応・
//! カンマ区切り・タグ参照のセグメント数）だけを見る - 意味検証は
//! [`crate::typecheck`] にまとめて置く。理由: 「未知関数」も「個数違い」も
//! 型検査と同じく「登録時に拒否する意味規則」であり、パーサとエラー生成元を
//! 分けると、将来関数を追加するときにパーサへ手を入れずに済む
//! （§4.2「組み込み追加で対応」との相性）。

use crate::ast::{BinOp, Expr, UnaryOp};
use crate::error::CompileError;
use crate::lexer::{Token, TokenKind};

pub fn parse(tokens: &[Token]) -> Result<Expr, CompileError> {
    let mut p = Parser { tokens, pos: 0 };
    let expr = p.parse_or()?;
    p.expect_eof()?;
    Ok(expr)
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect_eof(&mut self) -> Result<(), CompileError> {
        if self.peek().kind == TokenKind::Eof {
            Ok(())
        } else {
            Err(CompileError::Syntax {
                pos: self.peek().pos,
                message: format!(
                    "式の終端の後に余分なトークンがあります: {}",
                    describe(&self.peek().kind)
                ),
            })
        }
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> Result<Token, CompileError> {
        if &self.peek().kind == kind {
            Ok(self.advance())
        } else {
            Err(CompileError::Syntax {
                pos: self.peek().pos,
                message: format!(
                    "{what} が必要です（実際には {}）",
                    describe(&self.peek().kind)
                ),
            })
        }
    }

    // or_expr := and_expr ("||" and_expr)*
    fn parse_or(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.parse_and()?;
        while self.peek().kind == TokenKind::OrOr {
            let pos = lhs.pos();
            self.advance();
            let rhs = self.parse_and()?;
            lhs = Expr::Binary {
                op: BinOp::Or,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    // and_expr := equality ("&&" equality)*
    fn parse_and(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.parse_equality()?;
        while self.peek().kind == TokenKind::AndAnd {
            let pos = lhs.pos();
            self.advance();
            let rhs = self.parse_equality()?;
            lhs = Expr::Binary {
                op: BinOp::And,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    // equality := comparison (("==" | "!=") comparison)*
    fn parse_equality(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.parse_comparison()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::EqEq => BinOp::Eq,
                TokenKind::Ne => BinOp::Ne,
                _ => break,
            };
            let pos = lhs.pos();
            self.advance();
            let rhs = self.parse_comparison()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    // comparison := additive (("<" | ">" | "<=" | ">=") additive)*
    fn parse_comparison(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.parse_additive()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Lt => BinOp::Lt,
                TokenKind::Gt => BinOp::Gt,
                TokenKind::Le => BinOp::Le,
                TokenKind::Ge => BinOp::Ge,
                _ => break,
            };
            let pos = lhs.pos();
            self.advance();
            let rhs = self.parse_additive()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    // additive := multiplicative (("+" | "-") multiplicative)*
    fn parse_additive(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => break,
            };
            let pos = lhs.pos();
            self.advance();
            let rhs = self.parse_multiplicative()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    // multiplicative := unary (("*" | "/") unary)*
    fn parse_multiplicative(&mut self) -> Result<Expr, CompileError> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek().kind {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                _ => break,
            };
            let pos = lhs.pos();
            self.advance();
            let rhs = self.parse_unary()?;
            lhs = Expr::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
                pos,
            };
        }
        Ok(lhs)
    }

    // unary := ("-" | "!") unary | primary
    fn parse_unary(&mut self) -> Result<Expr, CompileError> {
        let pos = self.peek().pos;
        match self.peek().kind {
            TokenKind::Minus => {
                self.advance();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(inner),
                    pos,
                })
            }
            TokenKind::Bang => {
                self.advance();
                let inner = self.parse_unary()?;
                Ok(Expr::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(inner),
                    pos,
                })
            }
            _ => self.parse_primary(),
        }
    }

    // primary := number | "true" | "false" | tag_ref | call | "(" expr ")"
    fn parse_primary(&mut self) -> Result<Expr, CompileError> {
        let tok = self.peek().clone();
        match tok.kind {
            TokenKind::Num(v) => {
                self.advance();
                Ok(Expr::Num(v, tok.pos))
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_or()?;
                self.expect(&TokenKind::RParen, "')'")?;
                Ok(inner)
            }
            TokenKind::Ident(name) => {
                self.advance();
                if name == "true" {
                    return Ok(Expr::Bool(true, tok.pos));
                }
                if name == "false" {
                    return Ok(Expr::Bool(false, tok.pos));
                }
                if self.peek().kind == TokenKind::LParen {
                    return self.parse_call(name, tok.pos);
                }
                self.parse_tag_ref_rest(name, tok.pos)
            }
            _ => Err(CompileError::Syntax {
                pos: tok.pos,
                message: format!("式の開始として不正なトークンです: {}", describe(&tok.kind)),
            }),
        }
    }

    fn parse_call(&mut self, name: String, pos: usize) -> Result<Expr, CompileError> {
        self.expect(&TokenKind::LParen, "'('")?;
        let mut args = Vec::new();
        if self.peek().kind != TokenKind::RParen {
            args.push(self.parse_or()?);
            while self.peek().kind == TokenKind::Comma {
                self.advance();
                args.push(self.parse_or()?);
            }
        }
        self.expect(&TokenKind::RParen, "')'")?;
        Ok(Expr::Call { name, args, pos })
    }

    /// `first` は既に読んだ1つ目のセグメント。残り2セグメント
    /// （`.ident.ident`）を読み、ちょうど3セグメントであることを確認する。
    fn parse_tag_ref_rest(&mut self, first: String, pos: usize) -> Result<Expr, CompileError> {
        if self.peek().kind != TokenKind::Dot {
            return Err(CompileError::Syntax {
                pos: self.peek().pos,
                message: format!(
                    "識別子 '{first}' の後には関数呼び出しの '(' か、タグ参照の '.' が必要です"
                ),
            });
        }
        self.advance(); // '.'
        let second = self.expect_ident("タグ参照の第2セグメント")?;
        self.expect(&TokenKind::Dot, "タグ参照の2つ目の '.'")?;
        let third = self.expect_ident("タグ参照の第3セグメント")?;

        if self.peek().kind == TokenKind::Dot {
            return Err(CompileError::Syntax {
                pos: self.peek().pos,
                message:
                    "タグ参照は3セグメント（接続.グループ.タグ）までです - 余分な '.' があります"
                        .to_string(),
            });
        }

        let name = format!("{first}.{second}.{third}");
        Ok(Expr::TagRef { name, pos })
    }

    fn expect_ident(&mut self, what: &str) -> Result<String, CompileError> {
        match self.peek().kind.clone() {
            TokenKind::Ident(s) => {
                self.advance();
                Ok(s)
            }
            other => Err(CompileError::Syntax {
                pos: self.peek().pos,
                message: format!(
                    "{what} には識別子が必要です（実際には {}）",
                    describe(&other)
                ),
            }),
        }
    }
}

fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Num(v) => format!("数値 {v}"),
        TokenKind::Ident(s) => format!("識別子 '{s}'"),
        TokenKind::Plus => "'+'".to_string(),
        TokenKind::Minus => "'-'".to_string(),
        TokenKind::Star => "'*'".to_string(),
        TokenKind::Slash => "'/'".to_string(),
        TokenKind::EqEq => "'=='".to_string(),
        TokenKind::Ne => "'!='".to_string(),
        TokenKind::Lt => "'<'".to_string(),
        TokenKind::Gt => "'>'".to_string(),
        TokenKind::Le => "'<='".to_string(),
        TokenKind::Ge => "'>='".to_string(),
        TokenKind::AndAnd => "'&&'".to_string(),
        TokenKind::OrOr => "'||'".to_string(),
        TokenKind::Bang => "'!'".to_string(),
        TokenKind::LParen => "'('".to_string(),
        TokenKind::RParen => "')'".to_string(),
        TokenKind::Comma => "','".to_string(),
        TokenKind::Dot => "'.'".to_string(),
        TokenKind::Eof => "式の終端".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn parse_ok(source: &str) -> Expr {
        let tokens = tokenize(source).unwrap_or_else(|e| panic!("tokenize failed: {e:?}"));
        parse(&tokens).unwrap_or_else(|e| panic!("parse({source:?}) failed: {e:?}"))
    }

    fn parse_err(source: &str) -> CompileError {
        let tokens = tokenize(source).unwrap_or_else(|e| panic!("tokenize failed: {e:?}"));
        parse(&tokens).expect_err(&format!("expected parse({source:?}) to fail"))
    }

    #[test]
    fn precedence_multiplication_binds_tighter_than_addition() {
        // 1 + 2 * 3 == Binary(+, 1, Binary(*, 2, 3))
        let e = parse_ok("1 + 2 * 3");
        match e {
            Expr::Binary {
                op: BinOp::Add,
                lhs,
                rhs,
                ..
            } => {
                assert_eq!(*lhs, Expr::Num(1.0, 0));
                match *rhs {
                    Expr::Binary { op: BinOp::Mul, .. } => {}
                    other => panic!("expected Mul on rhs, got {other:?}"),
                }
            }
            other => panic!("expected top-level Add, got {other:?}"),
        }
    }

    #[test]
    fn precedence_unary_binds_tighter_than_multiplication() {
        // -x * y == Binary(*, Unary(-, x), y)  — 数値でテスト: -2 * 3
        let e = parse_ok("-2 * 3");
        match e {
            Expr::Binary {
                op: BinOp::Mul,
                lhs,
                ..
            } => match *lhs {
                Expr::Unary {
                    op: UnaryOp::Neg, ..
                } => {}
                other => panic!("expected Unary Neg on lhs, got {other:?}"),
            },
            other => panic!("expected top-level Mul, got {other:?}"),
        }
    }

    #[test]
    fn parens_override_precedence() {
        // (1 + 2) * 3 == Binary(*, Binary(+,1,2), 3)
        let e = parse_ok("(1 + 2) * 3");
        match e {
            Expr::Binary {
                op: BinOp::Mul,
                lhs,
                ..
            } => match *lhs {
                Expr::Binary { op: BinOp::Add, .. } => {}
                other => panic!("expected Add on lhs, got {other:?}"),
            },
            other => panic!("expected top-level Mul, got {other:?}"),
        }
    }

    /// 位置情報を無視した構造比較用に、全ノードの `pos` を0にする。
    fn strip_pos(expr: Expr) -> Expr {
        match expr {
            Expr::Num(v, _) => Expr::Num(v, 0),
            Expr::Bool(v, _) => Expr::Bool(v, 0),
            Expr::TagRef { name, .. } => Expr::TagRef { name, pos: 0 },
            Expr::Unary { op, expr, .. } => Expr::Unary {
                op,
                expr: Box::new(strip_pos(*expr)),
                pos: 0,
            },
            Expr::Binary { op, lhs, rhs, .. } => Expr::Binary {
                op,
                lhs: Box::new(strip_pos(*lhs)),
                rhs: Box::new(strip_pos(*rhs)),
                pos: 0,
            },
            Expr::Call { name, args, .. } => Expr::Call {
                name,
                args: args.into_iter().map(strip_pos).collect(),
                pos: 0,
            },
        }
    }

    #[test]
    fn parens_are_transparent_no_extra_ast_node() {
        // "(1)" と "1" は同じ形の AST になる（括弧は AST ノードを作らない
        // - 位置だけが異なるので、比較前に位置を正規化する）。
        assert_eq!(strip_pos(parse_ok("(1)")), strip_pos(parse_ok("1")));
    }

    #[test]
    fn logical_and_binds_tighter_than_or() {
        // a || b && c == Binary(||, a, Binary(&&, b, c))  — bool リテラルで検証
        let e = parse_ok("true || false && true");
        match e {
            Expr::Binary {
                op: BinOp::Or, rhs, ..
            } => match *rhs {
                Expr::Binary { op: BinOp::And, .. } => {}
                other => panic!("expected And on rhs, got {other:?}"),
            },
            other => panic!("expected top-level Or, got {other:?}"),
        }
    }

    #[test]
    fn comparison_binds_tighter_than_equality() {
        // 1 < 2 == 3 > 4 のような式は (1<2) == (3>4) と解釈される。
        let e = parse_ok("1 < 2 == 3 > 4");
        match e {
            Expr::Binary {
                op: BinOp::Eq,
                lhs,
                rhs,
                ..
            } => {
                assert!(matches!(*lhs, Expr::Binary { op: BinOp::Lt, .. }));
                assert!(matches!(*rhs, Expr::Binary { op: BinOp::Gt, .. }));
            }
            other => panic!("expected top-level Eq, got {other:?}"),
        }
    }

    #[test]
    fn unary_chains_allow_repeated_operators() {
        assert!(matches!(
            parse_ok("!!true"),
            Expr::Unary {
                op: UnaryOp::Not,
                ..
            }
        ));
        assert!(matches!(
            parse_ok("--5"),
            Expr::Unary {
                op: UnaryOp::Neg,
                ..
            }
        ));
    }

    #[test]
    fn tag_ref_parses_three_dotted_segments() {
        match parse_ok("calc.line1.temp_avg") {
            Expr::TagRef { name, pos } => {
                assert_eq!(name, "calc.line1.temp_avg");
                assert_eq!(pos, 0);
            }
            other => panic!("expected TagRef, got {other:?}"),
        }
    }

    #[test]
    fn tag_ref_segments_allow_hyphen_and_underscore() {
        match parse_ok("plc-1.grp_a.tag-name_1") {
            Expr::TagRef { name, .. } => assert_eq!(name, "plc-1.grp_a.tag-name_1"),
            other => panic!("expected TagRef, got {other:?}"),
        }
    }

    #[test]
    fn tag_ref_rejects_two_segments() {
        let err = parse_err("a.b");
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn tag_ref_rejects_four_segments() {
        let err = parse_err("a.b.c.d");
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn bare_identifier_without_dot_or_paren_is_syntax_error() {
        let err = parse_err("foo");
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn function_call_parses_generically_even_if_name_unknown() {
        // 関数名の妥当性は型検査の責務 - パーサは構文だけを見る。
        match parse_ok("foo(1, 2, 3)") {
            Expr::Call { name, args, .. } => {
                assert_eq!(name, "foo");
                assert_eq!(args.len(), 3);
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn function_call_with_zero_args() {
        match parse_ok("foo()") {
            Expr::Call { name, args, .. } => {
                assert_eq!(name, "foo");
                assert!(args.is_empty());
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn true_false_are_bool_literals_not_tag_refs() {
        assert_eq!(parse_ok("true"), Expr::Bool(true, 0));
        assert_eq!(parse_ok("false"), Expr::Bool(false, 0));
    }

    #[test]
    fn missing_closing_paren_is_syntax_error() {
        let err = parse_err("(1 + 2");
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn missing_comma_between_call_args_is_syntax_error() {
        let err = parse_err("if(1 2, 3)");
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn trailing_garbage_after_expression_is_syntax_error() {
        let err = parse_err("1 + 2 3");
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn empty_source_is_syntax_error() {
        let err = parse_err("");
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn nested_calls_parse() {
        match parse_ok("min(max(1, 2), abs(-3))") {
            Expr::Call { name, args, .. } => {
                assert_eq!(name, "min");
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], Expr::Call { name, .. } if name == "max"));
                assert!(matches!(&args[1], Expr::Call { name, .. } if name == "abs"));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn reparsing_pretty_printed_form_is_stable() {
        // 素朴な再パース安定性チェック: 同じソースを2回パースすれば同じ
        // AST になる（決定的パーサであることの確認）。
        for src in [
            "1 + 2 * 3",
            "if(a.b.c, 1, 2)",
            "bit(calc.x.y, 5) && !false",
            "(1 <= 2) == (3 >= 4)",
            "clamp(x.y.z, 0, 100)",
        ] {
            assert_eq!(parse_ok(src), parse_ok(src), "unstable reparse for {src}");
        }
    }
}
