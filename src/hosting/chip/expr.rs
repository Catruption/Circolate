//! Expression language shared by chip parsing and runtime.
//!
//! Grammar (precedence low -> high):
//!   ternary  `?:` (right assoc)
//!   `||`
//!   `&&`
//!   `== != < > <= >=`
//!   `|`
//!   `&`
//!   `<< >>`
//!   `+ -`
//!   `* /`
//!   `^` (right assoc, power)
//!   unary `- ! ~`
//!   primary (number, var, field[idx], table[idx], call, parens)
//!
//! Field/table ambiguity: `foo[bar]` is resolved at eval time. If `bar` is
//! a `Var` name and `foo` names a field in the context, it's a field
//! lookup. Otherwise if `foo` names a table and `bar` evaluates to an
//! integer, it's a table lookup.

use std::fmt;

#[derive(Clone, Debug)]
pub enum Expr {
    Num(f64),
    Var(String),
    /// `base[idx]`. Both possible meanings resolved at eval time.
    Index(String, Box<Expr>),
    Bin(Box<Expr>, BinOp, Box<Expr>),
    Un(UnOp, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add, Sub, Mul, Div, Pow,
    Eq, Ne, Lt, Gt, Le, Ge,
    And, Or,
    BitAnd, BitOr, Shl, Shr,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum UnOp { Neg, Not, BitNot }

#[derive(Debug)]
pub enum EvalError {
    UnknownVar(String),
    UnknownField(String),
    UnknownTable(String),
    UnknownFunction(String),
    WrongArgCount { func: String, expected: usize, got: usize },
    IndexOutOfRange { table: String, index: i64, len: usize },
    NotAnIndex(f64),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::UnknownVar(n) => write!(f, "unknown variable `{}`", n),
            EvalError::UnknownField(n) => write!(f, "unknown field `{}`", n),
            EvalError::UnknownTable(n) => write!(f, "unknown table `{}`", n),
            EvalError::UnknownFunction(n) => write!(f, "unknown function `{}`", n),
            EvalError::WrongArgCount { func, expected, got } =>
                write!(f, "`{}` expects {} args, got {}", func, expected, got),
            EvalError::IndexOutOfRange { table, index, len } =>
                write!(f, "table `{}` index {} out of range (len {})", table, index, len),
            EvalError::NotAnIndex(v) => write!(f, "non-integer index {}", v),
        }
    }
}

impl std::error::Error for EvalError {}

#[derive(Debug)]
pub struct ParseError {
    pub pos: usize,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at {}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for ParseError {}

// ───────────────────────── Context ─────────────────────────

pub trait Context {
    fn var(&self, name: &str) -> Option<f64>;
    /// `field[voice]`. `voice` is the literal identifier from the source.
    fn field(&self, field: &str, voice: &str) -> Option<f64>;
    fn table(&self, name: &str) -> Option<&[f64]>;
    /// `mask(reg)` — the bit mask for a register's width. Only used in
    /// bus expressions. Default: unknown.
    fn mask_of(&self, _reg: &str) -> Option<u32> { None }
}

pub struct EmptyContext;
impl Context for EmptyContext {
    fn var(&self, _: &str) -> Option<f64> { None }
    fn field(&self, _: &str, _: &str) -> Option<f64> { None }
    fn table(&self, _: &str) -> Option<&[f64]> { None }
}

// ───────────────────────── Eval ─────────────────────────

impl Expr {
    pub fn eval(&self, ctx: &dyn Context) -> Result<f64, EvalError> {
        match self {
            Expr::Num(n) => Ok(*n),
            Expr::Var(name) => ctx.var(name)
                .ok_or_else(|| EvalError::UnknownVar(name.clone())),
            Expr::Index(base, idx) => {
                // Prefer field interpretation when idx is a bare var name.
                if let Expr::Var(voice) = &**idx {
                    if let Some(v) = ctx.field(base, voice) {
                        return Ok(v);
                    }
                }
                let i = idx.eval(ctx)?;
                let i = i as i64;
                if (i as f64) != i as f64 { return Err(EvalError::NotAnIndex(i as f64)); }
                let tbl = ctx.table(base)
                    .ok_or_else(|| EvalError::UnknownTable(base.clone()))?;
                if i < 0 || i as usize >= tbl.len() {
                    return Err(EvalError::IndexOutOfRange {
                        table: base.clone(), index: i, len: tbl.len(),
                    });
                }
                Ok(tbl[i as usize])
            }
            Expr::Bin(a, op, b) => {
                let a = a.eval(ctx)?;
                let b = b.eval(ctx)?;
                Ok(match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    BinOp::Pow => a.powf(b),
                    BinOp::Eq  => (a == b) as i32 as f64,
                    BinOp::Ne  => (a != b) as i32 as f64,
                    BinOp::Lt  => (a <  b) as i32 as f64,
                    BinOp::Gt  => (a >  b) as i32 as f64,
                    BinOp::Le  => (a <= b) as i32 as f64,
                    BinOp::Ge  => (a >= b) as i32 as f64,
                    BinOp::And => ((a != 0.0) && (b != 0.0)) as i32 as f64,
                    BinOp::Or  => ((a != 0.0) || (b != 0.0)) as i32 as f64,
                    BinOp::BitAnd => ((a as i64) & (b as i64)) as f64,
                    BinOp::BitOr  => ((a as i64) | (b as i64)) as f64,
                    BinOp::Shl    => ((a as i64) << (b as i64)) as f64,
                    BinOp::Shr    => ((a as i64) >> (b as i64)) as f64,
                })
            }
            Expr::Un(op, e) => {
                let v = e.eval(ctx)?;
                Ok(match op {
                    UnOp::Neg => -v,
                    UnOp::Not => (v == 0.0) as i32 as f64,
                    UnOp::BitNot => (!(v as i64)) as f64,
                })
            }
            Expr::Ternary(c, t, f) => {
                if c.eval(ctx)? != 0.0 { t.eval(ctx) } else { f.eval(ctx) }
            }
            Expr::Call(name, args) => {
                match name.as_str() {
                    "pick" => {
                        if args.is_empty() {
                            return Err(EvalError::WrongArgCount {
                                func: "pick".into(), expected: 1, got: 0,
                            });
                        }
                        let sel = args[0].eval(ctx)? as usize;
                        if sel + 1 >= args.len() {
                            return Err(EvalError::WrongArgCount {
                                func: "pick".into(), expected: args.len(), got: sel + 2,
                            });
                        }
                        args[sel + 1].eval(ctx)
                    }
                    "mask" => {
                        // mask(reg) — depends on ctx knowing register widths.
                        if args.len() != 1 {
                            return Err(EvalError::WrongArgCount {
                                func: "mask".into(), expected: 1, got: args.len(),
                            });
                        }
                        if let Expr::Var(name) = &args[0] {
                            if let Some(m) = ctx.mask_of(name) {
                                return Ok(m as f64);
                            }
                        }
                        Err(EvalError::UnknownFunction("mask(...)".into()))
                    }
                    "parity" => {
                        if args.len() != 1 {
                            return Err(EvalError::WrongArgCount {
                                func: "parity".into(), expected: 1, got: args.len(),
                            });
                        }
                        let v = args[0].eval(ctx)? as u64;
                        Ok((v.count_ones() & 1) as f64)
                    }
                    "min" => {
                        let mut m = f64::INFINITY;
                        for a in args { m = m.min(a.eval(ctx)?); }
                        Ok(m)
                    }
                    "max" => {
                        let mut m = f64::NEG_INFINITY;
                        for a in args { m = m.max(a.eval(ctx)?); }
                        Ok(m)
                    }
                    other => Err(EvalError::UnknownFunction(other.into())),
                }
            }
        }
    }
}

// ───────────────────────── Tokenizer ─────────────────────────

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Plus, Minus, Star, Slash, Caret,
    LParen, RParen, LBracket, RBracket,
    Comma, Question, Colon,
    EqEq, Ne, Lt, Gt, Le, Ge,
    AndAnd, OrOr,
    Amp, Pipe, Shl, Shr, Tilde, Bang,
    End,
}

fn tokenize(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() { i += 1; continue; }
        let start = i;
        let tok = match c {
            b'0'..=b'9' => {
                let mut j = i;
                // hex
                if c == b'0' && i + 1 < b.len() && (b[i+1] == b'x' || b[i+1] == b'X') {
                    j = i + 2;
                    while j < b.len() && b[j].is_ascii_hexdigit() { j += 1; }
                    let s = std::str::from_utf8(&b[i+2..j]).unwrap();
                    let v = u64::from_str_radix(s, 16)
                    .map_err(|e: std::num::ParseIntError| ParseError { pos: i, msg: e.to_string() })?;
                    i = j;
                    Tok::Num(v as f64)
                } else if c == b'0' && i + 1 < b.len() && (b[i+1] == b'b' || b[i+1] == b'B') {
                    j = i + 2;
                    while j < b.len() && (b[j] == b'0' || b[j] == b'1') { j += 1; }
                    let s = std::str::from_utf8(&b[i+2..j]).unwrap();
                    let v = u64::from_str_radix(s, 2)
                    .map_err(|e: std::num::ParseIntError| ParseError { pos: i, msg: e.to_string() })?;
                    i = j;
                    Tok::Num(v as f64)
                } else {
                    while j < b.len() && b[j].is_ascii_digit() { j += 1; }
                    if j < b.len() && b[j] == b'.' {
                        j += 1;
                        while j < b.len() && b[j].is_ascii_digit() { j += 1; }
                    }
                    let s = std::str::from_utf8(&b[i..j]).unwrap();
                    let v: f64 = s.parse()
                    .map_err(|e: std::num::ParseFloatError| ParseError { pos: i, msg: e.to_string() })?;
                    i = j;
                    Tok::Num(v)
                }
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let mut j = i;
                while j < b.len()
                    && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.') {
                    j += 1;
                }
                let s = std::str::from_utf8(&b[i..j]).unwrap().to_string();
                i = j;
                Tok::Ident(s)
            }
            _ => {
                i += 1;
                match c {
                    b'+' => Tok::Plus,
                    b'-' => Tok::Minus,
                    b'*' => Tok::Star,
                    b'/' => Tok::Slash,
                    b'^' => Tok::Caret,
                    b'(' => Tok::LParen,
                    b')' => Tok::RParen,
                    b'[' => Tok::LBracket,
                    b']' => Tok::RBracket,
                    b',' => Tok::Comma,
                    b'?' => Tok::Question,
                    b':' => Tok::Colon,
                    b'=' => {
                        if i < b.len() && b[i] == b'=' { i += 1; Tok::EqEq }
                        else { return Err(ParseError { pos: start, msg: "expected `==`".into() }); }
                    }
                    b'!' => {
                        if i < b.len() && b[i] == b'=' { i += 1; Tok::Ne } else { Tok::Bang }
                    }
                    b'<' => {
                        if i < b.len() && b[i] == b'=' { i += 1; Tok::Le }
                        else if i < b.len() && b[i] == b'<' { i += 1; Tok::Shl }
                        else { Tok::Lt }
                    }
                    b'>' => {
                        if i < b.len() && b[i] == b'=' { i += 1; Tok::Ge }
                        else if i < b.len() && b[i] == b'>' { i += 1; Tok::Shr }
                        else { Tok::Gt }
                    }
                    b'&' => {
                        if i < b.len() && b[i] == b'&' { i += 1; Tok::AndAnd } else { Tok::Amp }
                    }
                    b'|' => {
                        if i < b.len() && b[i] == b'|' { i += 1; Tok::OrOr } else { Tok::Pipe }
                    }
                    b'~' => Tok::Tilde,
                    other => return Err(ParseError {
                        pos: start, msg: format!("unexpected byte `{}`", other as char),
                    }),
                }
            }
        };
        out.push((tok, start));
    }
    out.push((Tok::End, b.len()));
    Ok(out)
}

// ───────────────────────── Parser ─────────────────────────

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Tok { &self.toks[self.pos].0 }
    fn peek_pos(&self) -> usize { self.toks[self.pos].1 }
    fn bump(&mut self) -> Tok { let t = self.toks[self.pos].0.clone(); self.pos += 1; t }

    fn expect(&mut self, want: &Tok) -> Result<(), ParseError> {
        let here = self.peek_pos();
        let got = self.peek().clone();
        if &got == want { self.pos += 1; Ok(()) }
        else { Err(ParseError {
            pos: here, msg: format!("expected {:?}, got {:?}", want, got),
        }) }
    }

    fn parse_ternary(&mut self) -> Result<Expr, ParseError> {
        let cond = self.parse_or()?;
        if matches!(self.peek(), Tok::Question) {
            self.pos += 1;
            let then = self.parse_ternary()?;
            self.expect(&Tok::Colon)?;
            let else_ = self.parse_ternary()?;
            Ok(Expr::Ternary(Box::new(cond), Box::new(then), Box::new(else_)))
        } else {
            Ok(cond)
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_and()?;
        while matches!(self.peek(), Tok::OrOr) {
            self.pos += 1;
            let r = self.parse_and()?;
            l = Expr::Bin(Box::new(l), BinOp::Or, Box::new(r));
        }
        Ok(l)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_cmp()?;
        while matches!(self.peek(), Tok::AndAnd) {
            self.pos += 1;
            let r = self.parse_cmp()?;
            l = Expr::Bin(Box::new(l), BinOp::And, Box::new(r));
        }
        Ok(l)
    }

    fn parse_cmp(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_bitor()?;
        loop {
            let op = match self.peek() {
                Tok::EqEq => BinOp::Eq,
                Tok::Ne   => BinOp::Ne,
                Tok::Lt   => BinOp::Lt,
                Tok::Gt   => BinOp::Gt,
                Tok::Le   => BinOp::Le,
                Tok::Ge   => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let r = self.parse_bitor()?;
            l = Expr::Bin(Box::new(l), op, Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitor(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_bitand()?;
        while matches!(self.peek(), Tok::Pipe) {
            self.pos += 1;
            let r = self.parse_bitand()?;
            l = Expr::Bin(Box::new(l), BinOp::BitOr, Box::new(r));
        }
        Ok(l)
    }

    fn parse_bitand(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_shift()?;
        while matches!(self.peek(), Tok::Amp) {
            self.pos += 1;
            let r = self.parse_shift()?;
            l = Expr::Bin(Box::new(l), BinOp::BitAnd, Box::new(r));
        }
        Ok(l)
    }

    fn parse_shift(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_add()?;
        loop {
            let op = match self.peek() {
                Tok::Shl => BinOp::Shl,
                Tok::Shr => BinOp::Shr,
                _ => break,
            };
            self.pos += 1;
            let r = self.parse_add()?;
            l = Expr::Bin(Box::new(l), op, Box::new(r));
        }
        Ok(l)
    }

    fn parse_add(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Tok::Plus  => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let r = self.parse_mul()?;
            l = Expr::Bin(Box::new(l), op, Box::new(r));
        }
        Ok(l)
    }

    fn parse_mul(&mut self) -> Result<Expr, ParseError> {
        let mut l = self.parse_pow()?;
        loop {
            let op = match self.peek() {
                Tok::Star  => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                _ => break,
            };
            self.pos += 1;
            let r = self.parse_pow()?;
            l = Expr::Bin(Box::new(l), op, Box::new(r));
        }
        Ok(l)
    }

    fn parse_pow(&mut self) -> Result<Expr, ParseError> {
        let l = self.parse_unary()?;
        if matches!(self.peek(), Tok::Caret) {
            self.pos += 1;
            let r = self.parse_pow()?;
            return Ok(Expr::Bin(Box::new(l), BinOp::Pow, Box::new(r)));
        }
        Ok(l)
    }

    fn parse_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek().clone() {
            Tok::Minus => { self.pos += 1; Ok(Expr::Un(UnOp::Neg, Box::new(self.parse_unary()?))) }
            Tok::Bang  => { self.pos += 1; Ok(Expr::Un(UnOp::Not, Box::new(self.parse_unary()?))) }
            Tok::Tilde => { self.pos += 1; Ok(Expr::Un(UnOp::BitNot, Box::new(self.parse_unary()?))) }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match self.bump() {
            Tok::Num(v) => Ok(Expr::Num(v)),
            Tok::LParen => {
                let e = self.parse_ternary()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(name) => {
                if matches!(self.peek(), Tok::LParen) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Tok::RParen) {
                        loop {
                            args.push(self.parse_ternary()?);
                            if matches!(self.peek(), Tok::Comma) { self.pos += 1; continue; }
                            break;
                        }
                    }
                    self.expect(&Tok::RParen)?;
                    Ok(Expr::Call(name, args))
                } else if matches!(self.peek(), Tok::LBracket) {
                    self.pos += 1;
                    let idx = self.parse_ternary()?;
                    self.expect(&Tok::RBracket)?;
                    Ok(Expr::Index(name, Box::new(idx)))
                } else {
                    Ok(Expr::Var(name))
                }
            }
            other => Err(ParseError {
                pos: self.peek_pos(),
                msg: format!("unexpected token {:?}", other),
            }),
        }
    }
}

pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let toks = tokenize(src)?;
    let mut p = Parser { toks, pos: 0 };
    let e = p.parse_ternary()?;
    if !matches!(p.peek(), Tok::End) {
        return Err(ParseError {
            pos: p.peek_pos(),
            msg: format!("trailing tokens: {:?}", p.peek()),
        });
    }
    Ok(e)
}

// ───────────────────────── Tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct MapCtx {
        vars: std::collections::BTreeMap<String, f64>,
        tables: std::collections::BTreeMap<String, Vec<f64>>,
        fields: std::collections::BTreeMap<(String, String), f64>,
    }

    impl Context for MapCtx {
        fn var(&self, n: &str) -> Option<f64> { self.vars.get(n).copied() }
        fn field(&self, f: &str, v: &str) -> Option<f64> {
            self.fields.get(&(f.to_string(), v.to_string())).copied()
        }
        fn table(&self, n: &str) -> Option<&[f64]> { self.tables.get(n).map(|v| v.as_slice()) }
    }

    #[test]
    fn arithmetic() {
        let e = parse("clock / 16").unwrap();
        let mut ctx = MapCtx { vars: Default::default(), tables: Default::default(), fields: Default::default() };
        ctx.vars.insert("clock".into(), 3_579_545.0);
        assert_eq!(e.eval(&ctx).unwrap(), 3_579_545.0 / 16.0);
    }

    #[test]
    fn power_and_ternary() {
        let e = parse("(i == 15) ? 0 : 10 ^ (-i / 10)").unwrap();
        let mut ctx = MapCtx { vars: Default::default(), tables: Default::default(), fields: Default::default() };
        ctx.vars.insert("i".into(), 15.0);
        assert_eq!(e.eval(&ctx).unwrap(), 0.0);
        ctx.vars.insert("i".into(), 0.0);
        assert!((e.eval(&ctx).unwrap() - 1.0).abs() < 1e-12);
        ctx.vars.insert("i".into(), 10.0);
        assert!((e.eval(&ctx).unwrap() - 0.1).abs() < 1e-12);
    }

    #[test]
    fn field_lookup() {
        let e = parse("div[tone1]").unwrap();
        let mut ctx = MapCtx { vars: Default::default(), tables: Default::default(), fields: Default::default() };
        ctx.fields.insert(("div".into(), "tone1".into()), 200.0);
        assert_eq!(e.eval(&ctx).unwrap(), 200.0);
    }

    #[test]
    fn pick_builtin() {
        let e = parse("pick(2, 16, 32, 64, 200)").unwrap();
        let ctx = MapCtx { vars: Default::default(), tables: Default::default(), fields: Default::default() };
        assert_eq!(e.eval(&ctx).unwrap(), 64.0);
    }

    #[test]
    fn bitwise_in_bus_composition() {
        let e = parse("(reg & ~0x0F) | (lo & 0x0F)").unwrap();
        let mut ctx = MapCtx { vars: Default::default(), tables: Default::default(), fields: Default::default() };
        ctx.vars.insert("reg".into(), 0xFF00 as f64);
        ctx.vars.insert("lo".into(), 0x07 as f64);
        // (~0x0F) as i64 is -16; 0xFF00 & -16 = 0xFF00; | 7 = 0xFF07.
        assert_eq!(e.eval(&ctx).unwrap(), 0xFF07 as f64);
    }
}
