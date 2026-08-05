//! 字句解析。本文法は ASCII のみを対象とする（数値・識別子・演算子はすべて
//! ASCII 文字集合 - タグ名の分節規則も「英数字・`-`・`_`」で ASCII を想定
//! している、§10-12）。この前提により、ソース上の位置は常にバイト
//! オフセット = 文字オフセットになり、[`crate::error`] の `pos` に
//! 変換テーブルが要らない。ASCII 以外のバイトが現れた場合は
//! `CompileError::Syntax` にする（マルチバイト文字を含むタグ名は本文法の
//! 対象外 - I1 のタグ名は日本語も許すが、演算タグ・内部タグの外部名は
//! `calc`/`mem` 名前空間の予約セグメント設計（§4.2）が示すとおり ASCII
//! 識別子であることが前提になっている）。
//!
//! ## 識別子とハイフンの綱引き（ここで解決したパース上の曖昧性）
//!
//! §10-12 はタグ参照のセグメントに `-` を許す（`line-1` のような実在の
//! PLC 命名を通すため）が、`-` は同時に減算演算子でもある。本レキサは
//! 最長一致（maximal munch）で識別子を切り出すが、**末尾のハイフンは
//! 識別子に含めない**（先読みして次の文字が識別子継続文字でなければ
//! `-` は演算子として切り出す）。これにより:
//!
//! - `line-1.grp.tag`（ハイフンの後が数字）→ 識別子 `line-1` を維持
//! - `a.b.c-(x.y.z)`（ハイフンの後が `(`）→ `c` で識別子を終え、`-` は
//!   減算演算子
//! - `a.b.c-x.y.z`（ハイフンの後が識別子継続文字）→ `c-x` という1つの
//!   識別子に吸収される（減算のつもりなら空白か括弧で区切る必要がある）
//!
//! 最後のケースは本質的に曖昧（`c-x` が「タグ名の一部」か「c 引く x」かは
//! 空白なしでは判別不能）なので、レキサ側で機械的に「識別子優先」に
//! 倒し、ドキュメントで案内する（式の書き手が空白を入れれば必ず減算に
//! なる - 空白はどのトークンの間でも許され、識別子継続の判定に影響しない）。
//!
//! 識別子の先頭文字はハイフンにできない（`-` 単体、あるいは `-5` のような
//! 先頭ハイフンは常に単項マイナスとして扱う - タグ名がハイフンで始まる
//! ことはない前提、§10-12 の分節規則に合わせた制約）。

use crate::error::CompileError;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    EqEq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd,
    OrOr,
    Bang,
    LParen,
    RParen,
    Comma,
    Dot,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub pos: usize,
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, CompileError> {
    if !source.is_ascii() {
        return Err(CompileError::Syntax {
            pos: 0,
            message: "式は ASCII 文字のみ使用できます".to_string(),
        });
    }
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut tokens = Vec::new();
    let mut i = 0usize;

    while i < len {
        let c = bytes[i];

        if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            i += 1;
            continue;
        }

        let start = i;

        if c.is_ascii_digit() {
            let mut j = i;
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < len && bytes[j] == b'.' && j + 1 < len && bytes[j + 1].is_ascii_digit() {
                j += 1;
                while j < len && bytes[j].is_ascii_digit() {
                    j += 1;
                }
            }
            let text = &source[start..j];
            let value: f64 = text.parse().map_err(|_| CompileError::Syntax {
                pos: start,
                message: format!("数値リテラルとして解釈できません: '{text}'"),
            })?;
            tokens.push(Token {
                kind: TokenKind::Num(value),
                pos: start,
            });
            i = j;
            continue;
        }

        if is_ident_start(c) {
            let mut j = i + 1;
            loop {
                if j < len && is_ident_continue(bytes[j]) {
                    j += 1;
                } else if j < len
                    && bytes[j] == b'-'
                    && j + 1 < len
                    && is_ident_continue(bytes[j + 1])
                {
                    // 内部ハイフン（次の文字が識別子継続文字のときだけ吸収- モジュール doc 参照）。
                    j += 1;
                } else {
                    break;
                }
            }
            let text = source[start..j].to_string();
            tokens.push(Token {
                kind: TokenKind::Ident(text),
                pos: start,
            });
            i = j;
            continue;
        }

        macro_rules! two_char {
            ($second:expr, $two:expr, $one:expr) => {{
                if i + 1 < len && bytes[i + 1] == $second {
                    tokens.push(Token {
                        kind: $two,
                        pos: start,
                    });
                    i += 2;
                } else {
                    match $one {
                        Some(kind) => {
                            tokens.push(Token { kind, pos: start });
                            i += 1;
                        }
                        None => {
                            return Err(CompileError::Syntax {
                                pos: start,
                                message: format!(
                                    "'{}' の後には '{}' が必要です",
                                    c as char, $second as char
                                ),
                            })
                        }
                    }
                }
            }};
        }

        match c {
            b'+' => {
                tokens.push(Token {
                    kind: TokenKind::Plus,
                    pos: start,
                });
                i += 1;
            }
            b'-' => {
                tokens.push(Token {
                    kind: TokenKind::Minus,
                    pos: start,
                });
                i += 1;
            }
            b'*' => {
                tokens.push(Token {
                    kind: TokenKind::Star,
                    pos: start,
                });
                i += 1;
            }
            b'/' => {
                tokens.push(Token {
                    kind: TokenKind::Slash,
                    pos: start,
                });
                i += 1;
            }
            b'(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    pos: start,
                });
                i += 1;
            }
            b')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    pos: start,
                });
                i += 1;
            }
            b',' => {
                tokens.push(Token {
                    kind: TokenKind::Comma,
                    pos: start,
                });
                i += 1;
            }
            b'.' => {
                tokens.push(Token {
                    kind: TokenKind::Dot,
                    pos: start,
                });
                i += 1;
            }
            b'=' => two_char!(b'=', TokenKind::EqEq, None),
            b'!' => two_char!(b'=', TokenKind::Ne, Some(TokenKind::Bang)),
            b'<' => two_char!(b'=', TokenKind::Le, Some(TokenKind::Lt)),
            b'>' => two_char!(b'=', TokenKind::Ge, Some(TokenKind::Gt)),
            b'&' => two_char!(b'&', TokenKind::AndAnd, None),
            b'|' => two_char!(b'|', TokenKind::OrOr, None),
            other => {
                return Err(CompileError::Syntax {
                    pos: start,
                    message: format!("使用できない文字です: '{}'", other as char),
                })
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::Eof,
        pos: len,
    });
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        tokenize(source)
            .unwrap_or_else(|e| panic!("tokenize({source:?}) failed: {e:?}"))
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn tokenizes_integer_and_decimal_numbers() {
        assert_eq!(kinds("42"), vec![TokenKind::Num(42.0), TokenKind::Eof]);
        assert_eq!(kinds("3.5"), vec![TokenKind::Num(3.5), TokenKind::Eof]);
        assert_eq!(kinds("0.001"), vec![TokenKind::Num(0.001), TokenKind::Eof]);
    }

    #[test]
    fn number_stops_before_trailing_dot_without_digit() {
        // "3." - '.' はその後に数字が続かないので数値に含めない。
        assert_eq!(
            kinds("3."),
            vec![TokenKind::Num(3.0), TokenKind::Dot, TokenKind::Eof]
        );
    }

    #[test]
    fn tokenizes_all_single_and_double_char_operators() {
        assert_eq!(
            kinds("+ - * / == != < > <= >= && || !"),
            vec![
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::EqEq,
                TokenKind::Ne,
                TokenKind::Lt,
                TokenKind::Gt,
                TokenKind::Le,
                TokenKind::Ge,
                TokenKind::AndAnd,
                TokenKind::OrOr,
                TokenKind::Bang,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn tokenizes_parens_comma_dot() {
        assert_eq!(
            kinds("(a.b.c, 1)"),
            vec![
                TokenKind::LParen,
                TokenKind::Ident("a".to_string()),
                TokenKind::Dot,
                TokenKind::Ident("b".to_string()),
                TokenKind::Dot,
                TokenKind::Ident("c".to_string()),
                TokenKind::Comma,
                TokenKind::Num(1.0),
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn identifier_allows_internal_hyphen_and_underscore() {
        assert_eq!(
            kinds("line-1_a"),
            vec![TokenKind::Ident("line-1_a".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn trailing_hyphen_is_not_absorbed_into_identifier() {
        // 'c' の後のハイフンが '(' の直前 - 識別子継続文字が続かないので
        // ハイフンは演算子として切り出される（モジュール doc 参照）。
        assert_eq!(
            kinds("c-(x)"),
            vec![
                TokenKind::Ident("c".to_string()),
                TokenKind::Minus,
                TokenKind::LParen,
                TokenKind::Ident("x".to_string()),
                TokenKind::RParen,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn hyphen_between_two_identifier_like_words_is_absorbed() {
        // ドキュメント化された曖昧性の解決: 空白なしなら識別子優先。
        assert_eq!(
            kinds("b-tag"),
            vec![TokenKind::Ident("b-tag".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn minus_with_spaces_is_never_absorbed() {
        assert_eq!(
            kinds("a - b"),
            vec![
                TokenKind::Ident("a".to_string()),
                TokenKind::Minus,
                TokenKind::Ident("b".to_string()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn leading_underscore_identifier_ok() {
        assert_eq!(
            kinds("_mem"),
            vec![TokenKind::Ident("_mem".to_string()), TokenKind::Eof]
        );
    }

    #[test]
    fn identifier_cannot_start_with_digit() {
        // "1abc" は数値 1 のあと識別子 abc として別トークンになる（識別子
        // は先頭が数字であってはならない、というレキサの制約の帰結）。
        assert_eq!(
            kinds("1abc"),
            vec![
                TokenKind::Num(1.0),
                TokenKind::Ident("abc".to_string()),
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn whitespace_is_ignored_including_newlines_and_tabs() {
        assert_eq!(
            kinds("1\t+\n2 \r\n"),
            vec![
                TokenKind::Num(1.0),
                TokenKind::Plus,
                TokenKind::Num(2.0),
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn rejects_string_literal_double_quote() {
        let err = tokenize("\"hi\"").unwrap_err();
        assert!(matches!(err, CompileError::Syntax { pos: 0, .. }));
    }

    #[test]
    fn rejects_semicolon() {
        let err = tokenize("1;2").unwrap_err();
        assert!(matches!(err, CompileError::Syntax { pos: 1, .. }));
    }

    #[test]
    fn rejects_single_equals_assignment() {
        let err = tokenize("a = 1").unwrap_err();
        assert!(matches!(err, CompileError::Syntax { .. }));
    }

    #[test]
    fn rejects_single_ampersand_and_pipe() {
        assert!(tokenize("a & b").is_err());
        assert!(tokenize("a | b").is_err());
    }

    #[test]
    fn rejects_non_ascii_source() {
        let err = tokenize("1 + あ").unwrap_err();
        assert!(matches!(err, CompileError::Syntax { pos: 0, .. }));
    }

    #[test]
    fn rejects_unknown_character() {
        let err = tokenize("1 @ 2").unwrap_err();
        assert!(matches!(err, CompileError::Syntax { pos: 2, .. }));
    }

    #[test]
    fn error_position_is_byte_offset_of_offending_token() {
        let err = tokenize("1 + 2 $").unwrap_err();
        match err {
            CompileError::Syntax { pos, .. } => assert_eq!(pos, 6),
            other => panic!("expected Syntax, got {other:?}"),
        }
    }
}
