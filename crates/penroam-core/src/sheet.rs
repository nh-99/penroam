//! Spreadsheet cells in Markdown tables: values, formulas and checkboxes.
//!
//! A GFM table in a text block works as a small spreadsheet. Its cells keep
//! their Markdown source, and this module reads meaning into that source:
//!
//! * A cell whose text starts with `=` is a formula, such as `=SUM(B2:B5)` or
//!   `=B2*C2`. The finished layout ([`crate::markdown`]) draws its result in
//!   place of the source.
//! * A cell that is exactly `[ ]`, `[x]` or `[X]` is a checkbox.
//! * Other cells are values: numbers (`1,234.5`, `-3`, `$9.99`, `5 €`,
//!   `($3.00)`, `12%`),
//!   `TRUE`/`FALSE`, or text.
//!
//! Columns are lettered from `A` and rows numbered from `1`, which is the
//! header row, so the first data row is row 2. Formulas support `+ - * / ^`,
//! parentheses, cell references, ranges such as `A2:C4`, and the functions
//! `SUM`, `AVERAGE` (or `AVG`), `MIN`, `MAX`, `COUNT`, `PRODUCT`, `ROUND` and
//! `ABS`. Problems show as an error code in the cell: `#ERROR!` (can't
//! parse), `#NAME?` (unknown function), `#REF!` (outside the table),
//! `#VALUE!` (text where a number is needed), `#DIV/0!`, `#NUM!` (too large),
//! `#CYCLE!` (depends on itself) and `#LIMIT!` (ranges too large in total).
//!
//! The editor has a TypeScript port in `apps/desktop-mobile/src/editor/sheet.ts`.
//! Both are checked against `tests/fixtures/sheet-golden.json`, so a table
//! shows the same results on screen and in an exported PDF. Invariants:
//!
//! * Results don't depend on evaluation order. Formulas are evaluated in
//!   dependency order (Kahn's algorithm). Whatever is left when no formula is
//!   ready is on a cycle or depends on one, and shows `#CYCLE!`. There is no
//!   recursion across cells, so a long chain can't exhaust the stack.
//! * Work is bounded. Expression nesting is capped at [`MAX_NESTING`], and the
//!   cells all of a table's ranges cover are capped at [`MAX_RANGE_CELLS`];
//!   the formula that would pass the cap, and every later one with a range,
//!   shows `#LIMIT!`.
//! * Arithmetic is the same IEEE operations in the same order on both sides.
//!   Integer powers multiply by squaring rather than calling the platform's
//!   `pow`, and results are rounded and printed by [`format_number`], which
//!   uses no float formatting of the platform's.
//!
//! See `docs/formats/model-and-archive.md`.

/// Deepest nesting of parentheses, signs and function calls in a formula.
pub const MAX_NESTING: usize = 32;
/// Most cells the ranges in one table's formulas may cover, all told.
pub const MAX_RANGE_CELLS: usize = 200_000;

/// The currency symbols a number may start with.
pub const CURRENCIES: [char; 4] = ['$', '€', '£', '¥'];

/// How a number was written, so a result can be shown the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NumberFormat {
    /// Currency symbol; such numbers show two decimals.
    pub currency: Option<char>,
    /// The currency symbol comes after the digits (`5 €`).
    pub suffix: bool,
    /// A space separates the currency symbol from the digits.
    pub spaced: bool,
    /// Written with `%`; the value is a fraction (`12%` is 0.12).
    pub percent: bool,
    /// Written with thousands separators.
    pub grouped: bool,
    /// Digits written after the decimal point.
    pub decimals: usize,
}

impl NumberFormat {
    fn is_plain(&self) -> bool {
        self.currency.is_none() && !self.percent
    }
}

/// The meaning of a cell.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Nothing but whitespace.
    Blank,
    /// A number, with how it was written.
    Number(f64, NumberFormat),
    /// A checkbox, or `TRUE`/`FALSE`.
    Bool(bool),
    /// Anything else.
    Text(String),
    /// An error code such as `#REF!`.
    Error(&'static str),
}

const ERROR: &str = "#ERROR!";
const NAME: &str = "#NAME?";
const REF: &str = "#REF!";
const VALUE: &str = "#VALUE!";
const DIV0: &str = "#DIV/0!";
const NUM: &str = "#NUM!";
const CYCLE: &str = "#CYCLE!";
const LIMIT: &str = "#LIMIT!";

/// Whitespace trimmed from cells: what JavaScript's `trim` removes, which is
/// Unicode's White_Space without U+0085 and with U+FEFF.
fn is_trim(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// `raw` without the whitespace around it, trimmed as the TypeScript port's
/// Markdown parser trims a table cell.
pub fn trim(raw: &str) -> &str {
    raw.trim_matches(is_trim)
}

/// Emphasis wrapped around a whole cell, which a cell's value looks through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellStyle {
    /// `**…**` or `__…__`.
    pub bold: bool,
    /// `*…*` or `_…_`.
    pub italic: bool,
    /// `~~…~~`.
    pub strike: bool,
}

/// A cell's text without the emphasis wrapped around all of it, and that
/// emphasis, so `**$5**` is still the number 5 and `**=SUM(B2:B5)**` still a
/// formula. A marker counts only if it wraps the whole cell, appears nowhere
/// inside and doesn't touch a space, as Markdown would read it.
pub fn unstyle(raw: &str) -> (&str, CellStyle) {
    let mut s = trim(raw);
    let mut style = CellStyle::default();
    for _ in 0..3 {
        let found = ["**", "__", "~~", "*", "_"].into_iter().find_map(|m| {
            let inner = s.strip_prefix(m)?.strip_suffix(m)?;
            let ok = !inner.is_empty()
                && !inner.contains(m)
                && !inner.starts_with(is_trim)
                && !inner.ends_with(is_trim);
            ok.then_some((m, inner))
        });
        let Some((m, inner)) = found else { break };
        match m {
            "**" | "__" => style.bold = true,
            "~~" => style.strike = true,
            _ => style.italic = true,
        }
        s = inner;
    }
    (s, style)
}

/// `Some(ticked)` when the cell is a checkbox: exactly `[ ]`, `[x]` or `[X]`.
pub fn checkbox(raw: &str) -> Option<bool> {
    match raw.trim_matches(is_trim) {
        "[ ]" => Some(false),
        "[x]" | "[X]" => Some(true),
        _ => None,
    }
}

/// Reads a number as people write one in a spreadsheet: an optional sign, a
/// currency symbol before or after the digits (with or without a space), digits
/// with optional thousands separators, an optional fraction and an optional
/// `%`. Accounting style `($3.00)` is negative. `None` if `s` is anything else.
pub fn parse_number(s: &str) -> Option<(f64, NumberFormat)> {
    let all: Vec<char> = s.chars().collect();
    let bracketed = all.len() > 2 && all[0] == '(' && all[all.len() - 1] == ')';
    let chars = if bracketed {
        &all[1..all.len() - 1]
    } else {
        &all[..]
    };
    let skip_spaces = |mut i: usize| {
        while chars.get(i) == Some(&' ') {
            i += 1;
        }
        i
    };
    let mut i = 0;
    let mut neg = false;
    let mut signed = false;
    let mut fmt = NumberFormat::default();
    if let Some(&c @ ('+' | '-')) = chars.first() {
        neg = c == '-';
        signed = true;
        i += 1;
    }
    if let Some(&c) = chars.get(i) {
        if CURRENCIES.contains(&c) {
            fmt.currency = Some(c);
            let after = skip_spaces(i + 1);
            fmt.spaced = after > i + 1;
            i = after;
            if let (false, Some(&c @ ('+' | '-'))) = (signed, chars.get(i)) {
                neg = c == '-';
                signed = true;
                i += 1;
            }
        }
    }
    let int_start = i;
    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == ',') {
        i += 1;
    }
    let int: String = chars[int_start..i].iter().collect();
    if int.contains(',') {
        let groups: Vec<&str> = int.split(',').collect();
        let first = groups[0].len();
        if !(1..=3).contains(&first) || groups[1..].iter().any(|g| g.len() != 3) {
            return None;
        }
        fmt.grouped = true;
    }
    let mut frac = String::new();
    if chars.get(i) == Some(&'.') {
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() {
            frac.push(chars[i]);
            i += 1;
        }
        if frac.is_empty() {
            return None;
        }
    }
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    if chars.get(i) == Some(&'%') {
        fmt.percent = true;
        i += 1;
    }
    if fmt.currency.is_none() {
        let at = skip_spaces(i);
        if let Some(&c) = chars.get(at).filter(|c| CURRENCIES.contains(c)) {
            fmt.currency = Some(c);
            fmt.suffix = true;
            fmt.spaced = at > i;
            i = at + 1;
        }
    }
    // a sign inside brackets would make the brackets mean nothing
    if i != chars.len() || (fmt.percent && fmt.currency.is_some()) || (bracketed && signed) {
        return None;
    }
    neg |= bracketed;
    fmt.decimals = frac.len();
    let digits = format!(
        "{}.{}",
        if int.is_empty() {
            "0".to_owned()
        } else {
            int.replace(',', "")
        },
        if frac.is_empty() { "0" } else { &frac }
    );
    let mut n: f64 = digits.parse().ok()?;
    if fmt.percent {
        n /= 100.0;
    }
    Some((if neg { -n } else { n }, fmt))
}

/// The value of a cell that isn't a formula.
pub fn parse_value(raw: &str) -> Value {
    let s = unstyle(raw).0;
    if s.is_empty() {
        return Value::Blank;
    }
    if let Some(b) = checkbox(s) {
        return Value::Bool(b);
    }
    if s.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    match parse_number(s) {
        Some((n, f)) => Value::Number(n, f),
        None => Value::Text(s.to_owned()),
    }
}

/// Above this, an f64 no longer holds every integer, so digits can't be exact.
const EXACT: f64 = 9_007_199_254_740_992.0;

/// Prints a number the way `fmt` says it was written: currency with two
/// decimals, otherwise up to six with trailing zeros dropped (but at least as
/// many as were written), percentages times 100 with `%`, and thousands
/// separators when the source had them. Halves round away from zero.
/// Anything too large to print exactly is `#NUM!`.
pub fn format_number(n: f64, fmt: &NumberFormat) -> String {
    let v = if fmt.percent { n * 100.0 } else { n };
    if !v.is_finite() || v.abs() >= EXACT {
        return NUM.to_owned();
    }
    let (mut places, mut scale) = if fmt.currency.is_some() {
        (2, 100.0)
    } else {
        (6, 1e6)
    };
    if v.abs() * scale >= EXACT {
        places = 0;
        scale = 1.0;
    }
    let scaled = (v.abs() * scale + 0.5).floor();
    let digits = format!("{}", scaled as u64);
    let digits = format!("{digits:0>width$}", width = places + 1);
    let (int, frac) = digits.split_at(digits.len() - places);
    let mut frac = frac.to_owned();
    if fmt.currency.is_none() {
        let keep = fmt.decimals.min(places);
        while frac.len() > keep && frac.ends_with('0') {
            frac.pop();
        }
    }
    let mut out = String::new();
    if v < 0.0 && scaled > 0.0 {
        out.push('-');
    }
    if let (Some(c), false) = (fmt.currency, fmt.suffix) {
        out.push(c);
        if fmt.spaced {
            out.push(' ');
        }
    }
    if fmt.grouped {
        for (i, c) in int.chars().enumerate() {
            if i > 0 && (int.len() - i) % 3 == 0 {
                out.push(',');
            }
            out.push(c);
        }
    } else {
        out.push_str(int);
    }
    if !frac.is_empty() {
        out.push('.');
        out.push_str(&frac);
    }
    if fmt.percent {
        out.push('%');
    }
    if let (Some(c), true) = (fmt.currency, fmt.suffix) {
        if fmt.spaced {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// What a value shows as in a cell.
pub fn display(v: &Value) -> String {
    match v {
        Value::Blank => "0".to_owned(),
        Value::Number(n, f) => format_number(*n, f),
        Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
        Value::Text(t) => t.clone(),
        Value::Error(e) => (*e).to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Func {
    Sum,
    Average,
    Min,
    Max,
    Count,
    Product,
    Round,
    Abs,
}

impl Func {
    fn named(name: &str) -> Option<Func> {
        Some(match name.to_ascii_uppercase().as_str() {
            "SUM" => Func::Sum,
            "AVERAGE" | "AVG" => Func::Average,
            "MIN" => Func::Min,
            "MAX" => Func::Max,
            "COUNT" => Func::Count,
            "PRODUCT" => Func::Product,
            "ROUND" => Func::Round,
            "ABS" => Func::Abs,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
enum Expr {
    Num(f64),
    /// Row and column, from 0; may be outside the table.
    Ref(usize, usize),
    /// Top-left and bottom-right corners.
    Range(usize, usize, usize, usize),
    Neg(Box<Expr>),
    Bin(char, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    /// Row and column, from 0.
    Ref(usize, usize),
    Func(Func),
    Op(char),
}

/// Splits a formula (without its `=`) into tokens; `Err` is the error code.
fn tokens(src: &str) -> Result<Vec<Tok>, &'static str> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch == ' ' || ch == '\t' {
            i += 1;
        } else if ch.is_ascii_digit()
            || (ch == '.' && c.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            if c.get(i) == Some(&'.') && c.get(i + 1).is_some_and(char::is_ascii_digit) {
                i += 1;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = c[start..i].iter().collect();
            let text = if text.starts_with('.') {
                format!("0{text}")
            } else {
                text
            };
            out.push(Tok::Num(text.parse().map_err(|_| ERROR)?));
        } else if ch.is_ascii_alphabetic() {
            let start = i;
            while i < c.len() && c[i].is_ascii_alphabetic() {
                i += 1;
            }
            let letters: String = c[start..i].iter().collect();
            let digit_start = i;
            while i < c.len() && c[i].is_ascii_digit() {
                i += 1;
            }
            let digits: String = c[digit_start..i].iter().collect();
            let next = c.get(i).copied();
            if digits.is_empty() {
                if next != Some('(') {
                    return Err(NAME);
                }
                out.push(Tok::Func(Func::named(&letters).ok_or(NAME)?));
            } else {
                if letters.len() > 3
                    || digits.len() > 7
                    || next.is_some_and(|n| n.is_ascii_alphanumeric() || n == '_' || n == '(')
                {
                    return Err(NAME);
                }
                let col = letters.chars().fold(0usize, |n, l| {
                    n * 26 + (l.to_ascii_uppercase() as usize - 'A' as usize + 1)
                }) - 1;
                // row 0 is no row; usize::MAX is outside every table
                let row = digits
                    .parse::<usize>()
                    .map_err(|_| ERROR)?
                    .checked_sub(1)
                    .unwrap_or(usize::MAX);
                out.push(Tok::Ref(row, col));
            }
        } else if c[i..].starts_with(&['#', 'R', 'E', 'F', '!']) {
            // what deleting a referenced row or column leaves behind
            out.push(Tok::Ref(usize::MAX, 0));
            i += 5;
        } else if "+-*/^(),:".contains(ch) {
            out.push(Tok::Op(ch));
            i += 1;
        } else {
            return Err(ERROR);
        }
    }
    Ok(out)
}

/// Recursive descent over [`tokens`]; see the module docs for the grammar.
struct Parser {
    toks: Vec<Tok>,
    at: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.at)
    }

    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Tok::Op(op)) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn nest(&mut self) -> Result<(), &'static str> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            Err(ERROR)
        } else {
            Ok(())
        }
    }

    fn expr(&mut self) -> Result<Expr, &'static str> {
        let mut left = self.term()?;
        loop {
            let op = if self.eat('+') {
                '+'
            } else if self.eat('-') {
                '-'
            } else {
                return Ok(left);
            };
            left = Expr::Bin(op, Box::new(left), Box::new(self.term()?));
        }
    }

    fn term(&mut self) -> Result<Expr, &'static str> {
        let mut left = self.power()?;
        loop {
            let op = if self.eat('*') {
                '*'
            } else if self.eat('/') {
                '/'
            } else {
                return Ok(left);
            };
            left = Expr::Bin(op, Box::new(left), Box::new(self.power()?));
        }
    }

    fn power(&mut self) -> Result<Expr, &'static str> {
        let mut left = self.unary()?;
        while self.eat('^') {
            left = Expr::Bin('^', Box::new(left), Box::new(self.unary()?));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, &'static str> {
        let neg = if self.eat('-') {
            true
        } else if self.eat('+') {
            false
        } else {
            return self.primary();
        };
        self.nest()?;
        let inner = self.unary()?;
        self.depth -= 1;
        Ok(if neg {
            Expr::Neg(Box::new(inner))
        } else {
            inner
        })
    }

    fn primary(&mut self) -> Result<Expr, &'static str> {
        let tok = self.peek().cloned().ok_or(ERROR)?;
        self.at += 1;
        match tok {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Ref(r, c) => {
                if !self.eat(':') {
                    return Ok(Expr::Ref(r, c));
                }
                match self.peek().cloned() {
                    Some(Tok::Ref(r2, c2)) => {
                        self.at += 1;
                        Ok(Expr::Range(r.min(r2), c.min(c2), r.max(r2), c.max(c2)))
                    }
                    _ => Err(ERROR),
                }
            }
            Tok::Func(f) => {
                self.nest()?;
                if !self.eat('(') {
                    return Err(ERROR);
                }
                let mut args = Vec::new();
                if !self.eat(')') {
                    loop {
                        args.push(self.expr()?);
                        if self.eat(')') {
                            break;
                        }
                        if !self.eat(',') {
                            return Err(ERROR);
                        }
                    }
                }
                let arity_ok = match f {
                    Func::Round => (1..=2).contains(&args.len()),
                    Func::Abs => args.len() == 1,
                    _ => !args.is_empty(),
                };
                if !arity_ok {
                    return Err(ERROR);
                }
                self.depth -= 1;
                Ok(Expr::Call(f, args))
            }
            Tok::Op('(') => {
                self.nest()?;
                let inner = self.expr()?;
                if !self.eat(')') {
                    return Err(ERROR);
                }
                self.depth -= 1;
                Ok(inner)
            }
            Tok::Op(_) => Err(ERROR),
        }
    }
}

/// Parses a formula's source, with its leading `=`.
fn parse_formula(src: &str) -> Result<Expr, &'static str> {
    let body = unstyle(src).0.strip_prefix('=').ok_or(ERROR)?;
    let mut p = Parser {
        toks: tokens(body)?,
        at: 0,
        depth: 0,
    };
    let e = p.expr()?;
    if p.at != p.toks.len() {
        return Err(ERROR);
    }
    Ok(e)
}

/// The cells a formula reads, as (row, column) inside a table of
/// `rows` × `cols`. Ranges reaching outside the table read nothing (they are
/// `#REF!`). Each range cell counts towards `budget`; `None` if it runs out.
fn reads(
    e: &Expr,
    rows: usize,
    cols: usize,
    budget: &mut usize,
    out: &mut Vec<(usize, usize)>,
) -> Option<()> {
    match e {
        Expr::Num(_) => {}
        Expr::Ref(r, c) => {
            if *r < rows && *c < cols {
                out.push((*r, *c));
            }
        }
        Expr::Range(r0, c0, r1, c1) => {
            if *r1 < rows && *c1 < cols {
                let area = (r1 - r0 + 1) * (c1 - c0 + 1);
                *budget = budget.checked_sub(area)?;
                for r in *r0..=*r1 {
                    for c in *c0..=*c1 {
                        out.push((r, c));
                    }
                }
            }
        }
        Expr::Neg(a) => reads(a, rows, cols, budget, out)?,
        Expr::Bin(_, a, b) => {
            reads(a, rows, cols, budget, out)?;
            reads(b, rows, cols, budget, out)?;
        }
        Expr::Call(_, args) => {
            for a in args {
                reads(a, rows, cols, budget, out)?;
            }
        }
    }
    Some(())
}

fn has_range(e: &Expr) -> bool {
    match e {
        Expr::Range(..) => true,
        Expr::Num(_) | Expr::Ref(..) => false,
        Expr::Neg(a) => has_range(a),
        Expr::Bin(_, a, b) => has_range(a) || has_range(b),
        Expr::Call(_, args) => args.iter().any(has_range),
    }
}

/// `b` to the integer power `e` by squaring: the same multiplications in every port.
fn powi(b: f64, e: f64) -> f64 {
    let mut n = e.abs() as u64;
    let (mut result, mut base) = (1.0, b);
    while n > 0 {
        if n & 1 == 1 {
            result *= base;
        }
        base *= base;
        n >>= 1;
    }
    if e < 0.0 {
        1.0 / result
    } else {
        result
    }
}

/// `x` rounded to `places` decimals (negative: tens, hundreds...), halves away from zero.
fn round_to(x: f64, places: f64) -> f64 {
    let p = places.trunc().clamp(-10.0, 10.0);
    let f = powi(10.0, p.abs());
    let r = |v: f64| (v.abs() + 0.5).floor().copysign(v);
    if p >= 0.0 {
        r(x * f) / f
    } else {
        r(x / f) * f
    }
}

type Num = Result<(f64, NumberFormat), &'static str>;

/// Evaluates formulas against a table whose plain cells and finished formulas
/// are known.
struct Eval<'a> {
    cells: &'a [Vec<String>],
    cols: usize,
    /// Results of formula cells evaluated so far, by (row, column).
    done: &'a std::collections::HashMap<(usize, usize), Value>,
}

impl Eval<'_> {
    fn cell(&self, r: usize, c: usize) -> Value {
        if r >= self.cells.len() || c >= self.cols {
            return Value::Error(REF);
        }
        if let Some(v) = self.done.get(&(r, c)) {
            return v.clone();
        }
        match self.cells[r].get(c) {
            Some(raw) if is_formula(raw) => Value::Error(CYCLE),
            Some(raw) => parse_value(raw),
            None => Value::Blank,
        }
    }

    fn number(v: Value) -> Num {
        match v {
            Value::Blank => Ok((0.0, NumberFormat::default())),
            Value::Number(n, f) => Ok((n, f)),
            Value::Bool(b) => Ok((if b { 1.0 } else { 0.0 }, NumberFormat::default())),
            Value::Text(_) => Err(VALUE),
            Value::Error(e) => Err(e),
        }
    }

    fn num(&self, e: &Expr) -> Num {
        let (n, f) = match self.value(e) {
            Value::Number(n, f) => (n, f),
            v => Self::number(v)?,
        };
        if n.is_finite() {
            Ok((n, f))
        } else {
            Err(NUM)
        }
    }

    fn value(&self, e: &Expr) -> Value {
        match self.scalar(e) {
            Ok(v) => v,
            Err(code) => Value::Error(code),
        }
    }

    fn scalar(&self, e: &Expr) -> Result<Value, &'static str> {
        Ok(match e {
            Expr::Num(n) => Value::Number(*n, NumberFormat::default()),
            Expr::Ref(r, c) => self.cell(*r, *c),
            Expr::Range(..) => return Err(VALUE),
            Expr::Neg(a) => {
                let (n, f) = self.num(a)?;
                Value::Number(-n, f)
            }
            Expr::Bin(op, a, b) => {
                let (x, fx) = self.num(a)?;
                let (y, fy) = self.num(b)?;
                let pick = if fx.is_plain() { fy } else { fx };
                let (n, f) = match op {
                    '+' => (x + y, pick),
                    '-' => (x - y, pick),
                    '*' => (
                        x * y,
                        match (fx.percent, fy.percent) {
                            (true, false) => fy,
                            (false, true) => fx,
                            _ => pick,
                        },
                    ),
                    '/' => {
                        if y == 0.0 {
                            return Err(DIV0);
                        }
                        let same = fx.currency.is_some() && fx.currency == fy.currency;
                        (
                            x / y,
                            if same || fx.is_plain() {
                                NumberFormat::default()
                            } else {
                                fx
                            },
                        )
                    }
                    _ => {
                        let n = if y.fract() == 0.0 && y.abs() <= 1024.0 {
                            powi(x, y)
                        } else {
                            x.powf(y)
                        };
                        (n, fx)
                    }
                };
                if !n.is_finite() {
                    return Err(NUM);
                }
                Value::Number(n, f)
            }
            Expr::Call(func, args) => self.call(*func, args)?,
        })
    }

    /// The numbers an aggregate reads. References and ranges skip text,
    /// blanks and checkboxes, as spreadsheets do; other arguments must be
    /// numbers. Errors stop it, except for `COUNT`, which skips them.
    fn numbers(
        &self,
        args: &[Expr],
        count: bool,
    ) -> Result<Vec<(f64, NumberFormat)>, &'static str> {
        let mut out = Vec::new();
        for a in args {
            let area = match a {
                Expr::Range(r0, c0, r1, c1) => {
                    if *r1 >= self.cells.len() || *c1 >= self.cols {
                        if count {
                            continue;
                        }
                        return Err(REF);
                    }
                    Some((*r0, *c0, *r1, *c1))
                }
                Expr::Ref(r, c) => Some((*r, *c, *r, *c)),
                _ => None,
            };
            match area {
                Some((r0, c0, r1, c1)) => {
                    for r in r0..=r1 {
                        for c in c0..=c1 {
                            match self.cell(r, c) {
                                Value::Number(n, f) => out.push((n, f)),
                                Value::Error(e) if !count => return Err(e),
                                _ => {}
                            }
                        }
                    }
                }
                None => match self.num(a) {
                    Ok(n) => out.push(n),
                    Err(e) if !count => return Err(e),
                    Err(_) => {}
                },
            }
        }
        Ok(out)
    }

    fn call(&self, func: Func, args: &[Expr]) -> Result<Value, &'static str> {
        let finish = |n: f64, f: NumberFormat| {
            if n.is_finite() {
                Ok(Value::Number(n, f))
            } else {
                Err(NUM)
            }
        };
        match func {
            Func::Round | Func::Abs => {
                let (x, mut f) = self.num(&args[0])?;
                let n = if func == Func::Abs {
                    x.abs()
                } else {
                    let places = match args.get(1) {
                        Some(p) => self.num(p)?.0,
                        None => 0.0,
                    };
                    // shows the decimals it rounded to, not the ones it was given
                    f.decimals = places.trunc().clamp(0.0, 10.0) as usize;
                    round_to(x, places)
                };
                finish(n, f)
            }
            Func::Count => {
                let n = self.numbers(args, true)?.len();
                Ok(Value::Number(n as f64, NumberFormat::default()))
            }
            _ => {
                let nums = self.numbers(args, false)?;
                let f = nums.first().map(|n| n.1).unwrap_or_default();
                let mut it = nums.iter().map(|n| n.0);
                let n = match func {
                    Func::Sum => it.fold(0.0, |a, b| a + b),
                    Func::Product => match it.next() {
                        Some(first) => it.fold(first, |a, b| a * b),
                        None => 0.0,
                    },
                    Func::Min => it.reduce(f64::min).unwrap_or(0.0),
                    Func::Max => it.reduce(f64::max).unwrap_or(0.0),
                    _ => {
                        if nums.is_empty() {
                            return Err(DIV0);
                        }
                        nums.iter().fold(0.0, |a, b| a + b.0) / nums.len() as f64
                    }
                };
                finish(n, f)
            }
        }
    }
}

/// Whether a cell's source is a formula: its text, inside any emphasis, starts with `=`.
pub fn is_formula(raw: &str) -> bool {
    unstyle(raw).0.starts_with('=')
}

/// Evaluates every formula in a table. `cells` are the cells' source text,
/// row by row, the header first. The result has the same shape, with each
/// formula cell's result as it should be shown (a number, text or an error
/// code) and `None` for every other cell.
pub fn evaluate(cells: &[Vec<String>]) -> Vec<Vec<Option<String>>> {
    let values = evaluate_values(cells);
    values
        .into_iter()
        .map(|row| row.into_iter().map(|v| v.map(|v| display(&v))).collect())
        .collect()
}

/// [`evaluate`], giving each formula's [`Value`] rather than its text.
pub fn evaluate_values(cells: &[Vec<String>]) -> Vec<Vec<Option<Value>>> {
    use std::collections::{HashMap, VecDeque};
    let rows = cells.len();
    let cols = cells.iter().map(Vec::len).max().unwrap_or(0);
    let mut budget = MAX_RANGE_CELLS;
    let mut out: Vec<Vec<Option<Value>>> = cells.iter().map(|r| vec![None; r.len()]).collect();
    // formulas in reading order, and their parsed form or fixed result
    let mut formulas: Vec<(usize, usize)> = Vec::new();
    let mut parsed: Vec<Option<Expr>> = Vec::new();
    let mut index: HashMap<(usize, usize), usize> = HashMap::new();
    let mut done: HashMap<(usize, usize), Value> = HashMap::new();
    for (r, row) in cells.iter().enumerate() {
        for (c, raw) in row.iter().enumerate() {
            if !is_formula(raw) {
                continue;
            }
            index.insert((r, c), formulas.len());
            formulas.push((r, c));
            match parse_formula(raw) {
                Ok(e) => parsed.push(Some(e)),
                Err(code) => {
                    done.insert((r, c), Value::Error(code));
                    parsed.push(None);
                }
            }
        }
    }
    // the formulas each formula waits for, and the ones waiting for it
    let mut waiting = vec![0usize; formulas.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); formulas.len()];
    let mut exhausted = false;
    for (i, e) in parsed.iter_mut().enumerate() {
        let Some(expr) = e else { continue };
        let mut cells_read = Vec::new();
        if exhausted && has_range(expr)
            || reads(expr, rows, cols, &mut budget, &mut cells_read).is_none()
        {
            exhausted = true;
            done.insert(formulas[i], Value::Error(LIMIT));
            *e = None;
            continue;
        }
        for cell in cells_read {
            if let Some(&j) = index.get(&cell) {
                waiting[i] += 1;
                dependents[j].push(i);
            }
        }
    }
    let mut ready: VecDeque<usize> = (0..formulas.len()).filter(|&i| waiting[i] == 0).collect();
    let mut finished = vec![false; formulas.len()];
    while let Some(i) = ready.pop_front() {
        finished[i] = true;
        if let Some(expr) = &parsed[i] {
            let eval = Eval {
                cells,
                cols,
                done: &done,
            };
            let v = eval.value(expr);
            done.insert(formulas[i], v);
        }
        for &j in &dependents[i] {
            waiting[j] -= 1;
            if waiting[j] == 0 {
                ready.push_back(j);
            }
        }
    }
    for (i, &(r, c)) in formulas.iter().enumerate() {
        let v = if finished[i] {
            done.remove(&(r, c)).unwrap_or(Value::Error(CYCLE))
        } else {
            Value::Error(CYCLE)
        };
        out[r][c] = Some(v);
    }
    out
}
