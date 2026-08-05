//! パース結果の構文木。型検査前の生の木 - [`crate::typecheck`] が型付けと
//! 意味規則（関数名・引数個数・`bit()` の特殊制約）を検証する。ノードは
//! すべてソース上の位置（バイトオフセット）を保持し、型エラーの位置表示に
//! 使う。

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64, usize),
    Bool(bool, usize),
    /// 外部名そのまま（例: `"calc.line1.temp_avg"`）。3セグメントである
    /// ことはパーサが保証する。
    TagRef {
        name: String,
        pos: usize,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
        pos: usize,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        pos: usize,
    },
    /// 関数呼び出し。`name` は構文上任意の識別子を許す（未知関数か否かは
    /// パーサではなく型検査が判定する - `crate` トップレベル doc の
    /// 「なぜ関数名検証をパーサでなく型検査に置くか」参照）。
    Call {
        name: String,
        args: Vec<Expr>,
        pos: usize,
    },
}

impl Expr {
    /// このノードの位置（式の左端のバイトオフセット。二項演算子は演算子
    /// 自身ではなく左辺の開始位置 - エラーメッセージが式全体を指すように
    /// するため）。
    pub fn pos(&self) -> usize {
        match self {
            Expr::Num(_, pos) => *pos,
            Expr::Bool(_, pos) => *pos,
            Expr::TagRef { pos, .. } => *pos,
            Expr::Unary { pos, .. } => *pos,
            Expr::Binary { pos, .. } => *pos,
            Expr::Call { pos, .. } => *pos,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
}
