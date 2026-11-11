//! Spreadsheet values and formulas in Markdown tables, and the golden file the
//! TypeScript port is checked against (`apps/desktop-mobile/tests/sheet.test.ts`).
//! Run with `UPDATE_GOLDEN=1` after changing the rules on purpose.

use penroam_core::sheet::{
    checkbox, evaluate, format_number, parse_number, parse_value, NumberFormat, Value, MAX_NESTING,
    MAX_RANGE_CELLS,
};
use serde_json::{json, Value as Json};
use std::path::PathBuf;

fn table(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| r.iter().map(|c| (*c).to_owned()).collect())
        .collect()
}

/// The result of the formula in the last cell of a one-column table under
/// `cells`, which fill A2, A3, ...
fn eval(formula: &str, cells: &[&str]) -> String {
    let mut rows: Vec<Vec<String>> = vec![vec!["h".to_owned(), "f".to_owned()]];
    for c in cells {
        rows.push(vec![(*c).to_owned(), String::new()]);
    }
    rows.push(vec![String::new(), formula.to_owned()]);
    let out = evaluate(&rows);
    out.last().unwrap()[1]
        .clone()
        .expect("a formula cell has a result")
}

#[test]
fn numbers_read_as_written() {
    let plain = NumberFormat::default();
    assert_eq!(parse_number("42"), Some((42.0, plain)));
    assert_eq!(parse_number("-3.50").unwrap().0, -3.5);
    assert_eq!(parse_number("-3.50").unwrap().1.decimals, 2);
    assert_eq!(parse_number(".5").unwrap().0, 0.5);
    let (n, f) = parse_number("$1,234.5").unwrap();
    assert_eq!((n, f.currency, f.grouped), (1234.5, Some('$'), true));
    assert_eq!(parse_number("-€5").unwrap().0, -5.0);
    assert_eq!(parse_number("£-5").unwrap().0, -5.0);
    let (n, f) = parse_number("$ 12").unwrap();
    assert_eq!(
        (n, f.currency, f.spaced, f.suffix),
        (12.0, Some('$'), true, false)
    );
    let (n, f) = parse_number("1.25$").unwrap();
    assert_eq!(
        (n, f.currency, f.spaced, f.suffix),
        (1.25, Some('$'), false, true)
    );
    let (n, f) = parse_number("-1 234,5 €".replace(' ', "").replace(',', ".").as_str()).unwrap();
    assert_eq!((n, f.suffix), (-1234.5, true));
    assert!(parse_number("5 €").unwrap().1.spaced);
    assert_eq!(parse_number("($3.00)").unwrap().0, -3.0);
    assert_eq!(parse_number("(12%)").unwrap().0, -0.12);
    let (n, f) = parse_number("12.5%").unwrap();
    assert!((n - 0.125).abs() < 1e-12 && f.percent);
    for bad in [
        "", "-", "1,23", "12,3456", "1.", "1.2.3", "$5%", "5 apples", "1e5", "--5", "$", "$5$",
        "(-5)", "()", "(5", "5 % ", "USD 5", "$  ",
    ] {
        assert_eq!(parse_number(bad), None, "{bad}");
    }
}

#[test]
fn emphasis_around_a_cell_is_looked_through() {
    use penroam_core::sheet::{is_formula, unstyle, CellStyle};
    assert_eq!(parse_value("**$5**"), parse_value("$5"));
    assert_eq!(parse_value("_12%_"), parse_value("12%"));
    assert_eq!(
        unstyle(" ***~~x~~*** ").1,
        CellStyle {
            bold: true,
            italic: true,
            strike: true
        }
    );
    assert!(is_formula("**=SUM(A1:A2)**"));
    for text in ["**a** and **b**", "** 5 **", "*a*b*", "****"] {
        assert_eq!(unstyle(text).0, text.trim(), "{text}");
    }
}

#[test]
fn values_and_checkboxes() {
    assert_eq!(parse_value("  "), Value::Blank);
    assert_eq!(parse_value("[x]"), Value::Bool(true));
    assert_eq!(parse_value("[ ]"), Value::Bool(false));
    assert_eq!(parse_value("True"), Value::Bool(true));
    assert_eq!(parse_value("FALSE"), Value::Bool(false));
    assert_eq!(parse_value(" pear "), Value::Text("pear".into()));
    assert_eq!(checkbox(" [X] "), Some(true));
    assert_eq!(checkbox("[x] done"), None);
    assert_eq!(checkbox("[]"), None);
}

#[test]
fn formatting_keeps_the_way_numbers_were_written() {
    let plain = NumberFormat::default();
    let usd = NumberFormat {
        currency: Some('$'),
        ..plain
    };
    assert_eq!(format_number(1.5, &plain), "1.5");
    assert_eq!(format_number(1.0 / 3.0, &plain), "0.333333");
    assert_eq!(format_number(-0.0000001, &plain), "0");
    assert_eq!(
        format_number(
            2.0,
            &NumberFormat {
                decimals: 2,
                ..plain
            }
        ),
        "2.00"
    );
    assert_eq!(format_number(1234.5, &usd), "$1234.50");
    assert_eq!(
        format_number(
            -1234.5,
            &NumberFormat {
                grouped: true,
                ..usd
            }
        ),
        "-$1,234.50"
    );
    assert_eq!(
        format_number(
            0.125,
            &NumberFormat {
                percent: true,
                ..plain
            }
        ),
        "12.5%"
    );
    assert_eq!(format_number(0.0000005, &plain), "0.000001");
    assert_eq!(format_number(1e16, &plain), "#NUM!");
    assert_eq!(format_number(1e12, &plain), "1000000000000");
    assert_eq!(format_number(f64::NAN, &plain), "#NUM!");
    let eur = NumberFormat {
        currency: Some('€'),
        suffix: true,
        spaced: true,
        ..plain
    };
    assert_eq!(format_number(-5.0, &eur), "-5.00 €");
    assert_eq!(
        format_number(
            5.0,
            &NumberFormat {
                spaced: true,
                ..usd
            }
        ),
        "$ 5.00"
    );
}

#[test]
fn arithmetic_and_functions() {
    assert_eq!(eval("=1+2*3", &[]), "7");
    assert_eq!(eval("=(1+2)*3", &[]), "9");
    assert_eq!(eval("=-2^2", &[]), "4");
    assert_eq!(eval("=2^-1", &[]), "0.5");
    assert_eq!(eval("=2^3^2", &[]), "64");
    assert_eq!(eval("=10/4", &[]), "2.5");
    assert_eq!(eval("=SUM(A2:A4)", &["1", "2", "3"]), "6");
    assert_eq!(eval("=sum(a2:a4)", &["1", "x", "3"]), "4");
    assert_eq!(eval("=AVERAGE(A2:A3)", &["1", "2"]), "1.5");
    assert_eq!(eval("=AVG(A2:A3)", &["1", "2"]), "1.5");
    assert_eq!(eval("=MIN(A2:A4)", &["5", "-2", "3"]), "-2");
    assert_eq!(eval("=MAX(A2:A4, 10)", &["5", "-2", "3"]), "10");
    assert_eq!(eval("=COUNT(A2:A5)", &["5", "x", "", "3"]), "2");
    assert_eq!(eval("=PRODUCT(A2:A3)", &["4", "2.5"]), "10");
    assert_eq!(eval("=ROUND(A2, 1)", &["2.25"]), "2.3");
    assert_eq!(eval("=ROUND(A2)", &["-2.5"]), "-3");
    assert_eq!(eval("=ROUND(1234, -2)", &[]), "1200");
    assert_eq!(eval("=ABS(A2)", &["-7"]), "7");
    assert_eq!(eval("=A2", &["pear"]), "pear");
    assert_eq!(eval("=A2", &[""]), "0");
    assert_eq!(eval("=A2", &["[x]"]), "TRUE");
    assert_eq!(eval("=A2+1", &["[x]"]), "2");
    assert_eq!(eval("=SUM(A2:A3)", &["[x]", "4"]), "4");
}

#[test]
fn results_take_the_format_of_their_inputs() {
    assert_eq!(eval("=SUM(A2:A3)", &["$1.25", "2"]), "$3.25");
    assert_eq!(eval("=SUM(A2:A3)", &["1,000", "2,000"]), "3,000");
    assert_eq!(eval("=A2*A3", &["10%", "$200"]), "$20.00");
    assert_eq!(eval("=A2/A3", &["$10", "$4"]), "2.5");
    assert_eq!(eval("=A2+A3", &["10%", "5%"]), "15%");
    assert_eq!(eval("=COUNT(A2:A3)", &["$1", "$2"]), "2");
}

#[test]
fn errors_say_what_went_wrong() {
    assert_eq!(eval("=", &[]), "#ERROR!");
    assert_eq!(eval("=1+", &[]), "#ERROR!");
    assert_eq!(eval("=(1", &[]), "#ERROR!");
    assert_eq!(eval("=1 2", &[]), "#ERROR!");
    assert_eq!(eval("=1 & 2", &[]), "#ERROR!");
    assert_eq!(eval("=ROUND(1,2,3)", &[]), "#ERROR!");
    assert_eq!(eval("=SUM()", &[]), "#ERROR!");
    assert_eq!(eval("=FOO(1)", &[]), "#NAME?");
    assert_eq!(eval("=pear", &[]), "#NAME?");
    assert_eq!(eval("=A1B", &[]), "#NAME?");
    assert_eq!(eval("=Z9", &[]), "#REF!");
    assert_eq!(eval("=A0", &[]), "#REF!");
    assert_eq!(eval("=#REF!*2", &[]), "#REF!");
    assert_eq!(eval("=SUM(A2:A99)", &["1"]), "#REF!");
    assert_eq!(eval("=A2+1", &["pear"]), "#VALUE!");
    assert_eq!(eval("=A2:A3", &["1", "2"]), "#VALUE!");
    assert_eq!(eval("=1/0", &[]), "#DIV/0!");
    assert_eq!(eval("=AVERAGE(A2)", &["x"]), "#DIV/0!");
    assert_eq!(eval("=10^400", &[]), "#NUM!");
    assert_eq!(eval("=A2+1", &["=1/0"]), "#DIV/0!");
    assert_eq!(eval("=COUNT(A2:A3)", &["=1/0", "4"]), "1");
    let deep = format!(
        "={}1{}",
        "(".repeat(MAX_NESTING + 1),
        ")".repeat(MAX_NESTING + 1)
    );
    assert_eq!(eval(&deep, &[]), "#ERROR!");
    let ok = format!("={}1{}", "(".repeat(MAX_NESTING), ")".repeat(MAX_NESTING));
    assert_eq!(eval(&ok, &[]), "1");
}

#[test]
fn cycles_are_found_without_recursion() {
    let t = table(&[
        &["a", "b"],
        &["=B2", "=A2"],
        &["=A2+1", "5"],
        &["=SUM(A2:A4)", ""],
    ]);
    let out = evaluate(&t);
    assert_eq!(out[1][0].as_deref(), Some("#CYCLE!"));
    assert_eq!(out[1][1].as_deref(), Some("#CYCLE!"));
    assert_eq!(out[2][0].as_deref(), Some("#CYCLE!"));
    assert_eq!(out[3][0].as_deref(), Some("#CYCLE!"));
    assert_eq!(out[2][1], None);
    // a chain far longer than any stack allows
    let mut rows = vec![vec!["n".to_owned()], vec!["1".to_owned()]];
    for r in 2..20_000 {
        rows.push(vec![format!("=A{r}+1")]);
    }
    let out = evaluate(&rows);
    assert_eq!(out.last().unwrap()[0].as_deref(), Some("19999"));
}

#[test]
fn range_work_is_bounded() {
    let side = 500;
    let mut rows: Vec<Vec<String>> = (0..side).map(|_| vec!["1".to_owned(); 2]).collect();
    // each covers side × 2 cells: the budget runs out part way down
    for row in rows.iter_mut().skip(1) {
        row[1] = format!("=SUM(A1:A{side})+COUNT(A1:A{side})");
    }
    let out = evaluate(&rows);
    let per = 2 * side;
    let fit = MAX_RANGE_CELLS / per;
    assert_eq!(out[fit][1].as_deref(), Some("1000"));
    assert_eq!(out[fit + 1][1].as_deref(), Some("#LIMIT!"));
    assert_eq!(out[side - 1][1].as_deref(), Some("#LIMIT!"));
}

/// Tables run through both ports; their results must agree.
const CORPUS: &[&[&[&str]]] = &[
    &[
        &["Item", "Qty", "Price", "Total"],
        &["Apples", "3", "$0.50", "=B2*C2"],
        &["Pears", "12", "$1.25", "=B3*C3"],
        &["Figs", "1,200", "$0.05", "=B4*C4"],
        &["Sum", "=SUM(B2:B4)", "=AVERAGE(C2:C4)", "=SUM(D2:D4)"],
    ],
    &[
        &["a", "b", "c"],
        &["1", "2", "=A2/B2"],
        &["10%", "25%", "=A3+B3"],
        &["-3.25", "4", "=ROUND(A4*B4, 1)"],
        &["=MIN(A2:B4)", "=MAX(A2:B4)", "=COUNT(A2:C4)"],
    ],
    &[
        &["x", "y"],
        &["[x]", "=A2+0"],
        &["[ ]", "=A3"],
        &["TRUE", "=SUM(A2:A4)"],
        &["pear", "=A5"],
        &["", "=A6"],
    ],
    &[
        &["e", "f"],
        &["=1/0", "=A2+1"],
        &["=FOO(1)", "=Z99"],
        &["=1+", "=A2:A3"],
        &["=B5", "=A5"],
        &["=2^0.5", "=2^10"],
        &["=0.1+0.2", "=1/3"],
        &["=1e5", "=10^20"],
        &["=#REF!+1", "=SUM(A2:#REF!)"],
        &["=-(-(-1))", "= 3 * ( 4 - 1 ) "],
        &["=PRODUCT(A7:B7)", "=ABS(-0.0000004)"],
        &["=ROUND(2.5)", "=ROUND(-2.5)"],
        &["=ROUND(1.005, 2)", "=ROUND(123.456, -1)"],
        &["€3", "=A13*3"],
        &["£1,000.10", "=A14-0.1"],
        &["¥5", "=SUM(A13:A15)"],
        &["5 €", "=A17*2"],
        &["$ 12", "=A18+A17"],
        &["($3.50)", "=SUM(A17:A19)"],
        &["1.25$", "=A20*4"],
        &["**$2**", "**=A22*2**"],
        &["*~~3~~*", "=A23+A22"],
        &["**a**b**", "~~=A22~~"],
        &["** 4 **", "_=A22_"],
    ],
    &[&["only a header", "=1+1"]],
    &[&["ragged", "row"], &["=B3"], &["1", "2"]],
];

fn golden_cases() -> Vec<Json> {
    CORPUS
        .iter()
        .map(|t| {
            let cells = table(t);
            json!({ "cells": cells, "results": evaluate(&cells) })
        })
        .collect()
}

#[test]
fn golden_sheet() {
    let actual = Json::Array(golden_cases());
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sheet-golden.json");
    if std::env::var("UPDATE_GOLDEN").is_ok() || !path.exists() {
        let compact: Vec<String> = actual
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c.to_string())
            .collect();
        std::fs::write(&path, format!("[\n{}\n]\n", compact.join(",\n"))).unwrap();
    }
    let golden: Json = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(golden, actual);
}
