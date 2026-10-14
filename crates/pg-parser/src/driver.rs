//! The LALR(1) driver: a port of the deterministic part of bison 2.3's `yacc.c`
//! skeleton (`yyparse`), running the tables extracted into [`crate::tables`].
//!
//! DuckDB's grammar has no `error` productions and `yyerror` always throws, so the
//! first syntax error ends the parse; error recovery is not needed.

use rustdb_common::Result;

use crate::actions::RULE_ACTIONS;
use crate::error::internal;
use crate::nodes::{List, Node};
use crate::scanner::Scanner;
use crate::semantic::{Reduction, RuleAction, SemValue};
use crate::tables::{
    YYCHECK, YYDEFACT, YYDEFGOTO, YYFINAL, YYINITDEPTH, YYLAST, YYMAXDEPTH, YYMAXUTOK, YYNTOKENS, YYPACT,
    YYPACT_NINF, YYPGOTO, YYR1, YYR2, YYTABLE, YYTABLE_NINF, YYTRANSLATE, YYUNDEFTOK,
};

/// `YYTRANSLATE`: maps a token code to a grammar symbol number.
fn translate(code: u16) -> usize {
    let code = usize::from(code);
    if code <= YYMAXUTOK {
        usize::from(YYTRANSLATE[code])
    } else {
        YYUNDEFTOK
    }
}

/// Looks up `table[index]` in the packed action/goto table if `check` confirms the
/// entry belongs to `expected`.
fn packed(index: i64, expected: usize) -> Option<i16> {
    let i = usize::try_from(index).ok().filter(|&i| i <= YYLAST)?;
    (usize::try_from(YYCHECK[i]).ok() == Some(expected)).then(|| YYTABLE[i])
}

/// The lookahead token (`yychar`, `yylval`, `yylloc`).
struct Lookahead<'a> {
    symbol: usize,
    value: SemValue<'a>,
    loc: i32,
}

/// Runs the parser over the whole input and returns the statement list (the value
/// of `stmtblock`).
pub(crate) fn parse<'a>(scanner: &mut Scanner<'a>) -> Result<List> {
    let mut states: Vec<usize> = Vec::with_capacity(YYINITDEPTH);
    let mut values: Vec<SemValue<'a>> = Vec::with_capacity(YYINITDEPTH);
    let mut locations: Vec<i32> = Vec::with_capacity(YYINITDEPTH);
    // The bottom value/location slots are never read (bison "wastes" them).
    states.push(0);
    values.push(SemValue::None);
    locations.push(0);
    let mut lookahead: Option<Lookahead<'a>> = None;
    let mut eof = false;

    loop {
        let state = states.last().copied().unwrap_or(0);
        if states.len() >= YYMAXDEPTH {
            return Err(scanner.syntax_error("memory exhausted"));
        }

        // yybackup: decide between shift, reduce and error.
        let pact = YYPACT[state];
        let mut rule = None;
        if pact != YYPACT_NINF {
            if lookahead.is_none() {
                let tok = scanner.next_token()?;
                let symbol = if tok.code == 0 { 0 } else { translate(tok.code) };
                eof = tok.code == 0;
                let loc = i32::try_from(tok.loc).map_err(|_| internal("query too long"))?;
                lookahead = Some(Lookahead {
                    symbol,
                    value: tok.value,
                    loc,
                });
            }
            let symbol = lookahead.as_ref().map_or(0, |la| la.symbol);
            match packed(i64::from(pact) + symbol as i64, symbol) {
                None => {}
                Some(action) if action <= 0 => {
                    if action == 0 || i32::from(action) == YYTABLE_NINF {
                        return Err(scanner.syntax_error("syntax error"));
                    }
                    rule = Some(usize::from(action.unsigned_abs()));
                }
                Some(action) => {
                    let target = usize::from(action.unsigned_abs());
                    if target == YYFINAL {
                        break;
                    }
                    // Shift the lookahead (end of input is never consumed).
                    let la = if eof {
                        Lookahead {
                            symbol: 0,
                            value: SemValue::None,
                            loc: lookahead.as_ref().map_or(0, |l| l.loc),
                        }
                    } else {
                        lookahead.take().ok_or_else(|| internal("missing lookahead"))?
                    };
                    states.push(target);
                    values.push(la.value);
                    locations.push(la.loc);
                    continue;
                }
            }
        }

        // yydefault: the state's default reduction.
        let rule = match rule {
            Some(rule) => rule,
            None => match usize::from(YYDEFACT[state]) {
                0 => return Err(scanner.syntax_error("syntax error")),
                rule => rule,
            },
        };

        // yyreduce
        let len = usize::from(YYR2[rule]);
        let base = states.len() - len;
        let (value, location) = {
            let mut reduction = Reduction::new(&mut values[base..], &locations[base..], scanner, rule);
            let value = match RULE_ACTIONS[rule] {
                RuleAction::Default => reduction.take_raw(1),
                RuleAction::Call(action, alt) => action(&mut reduction, alt)?,
            };
            (value, reduction.location)
        };
        states.truncate(base);
        values.truncate(base);
        locations.truncate(base);
        values.push(value);
        locations.push(location);

        // Goto the state for the reduced nonterminal.
        let lhs = usize::from(YYR1[rule]) - YYNTOKENS;
        let top = states.last().copied().unwrap_or(0);
        let next = match packed(i64::from(YYPGOTO[lhs]) + top as i64, top) {
            Some(target) => usize::from(target.unsigned_abs()),
            None => usize::try_from(YYDEFGOTO[lhs]).map_err(|_| internal("bad goto"))?,
        };
        states.push(next);
    }

    // Accepted: the value of `stmtblock` sits above the bottom slot.
    match values.pop() {
        Some(SemValue::List(list)) => Ok(list),
        Some(SemValue::None) => Ok(List::new()),
        _ => Err(internal("parser accepted without a statement list")),
    }
}

/// Unwraps the raw statements built by `stmtmulti`.
pub(crate) fn into_raw_statements(list: List) -> Result<Vec<crate::RawStmt>> {
    list.into_iter()
        .map(|node| match node {
            Node::RawStmt(stmt) => Ok(*stmt),
            other => Err(internal(format!("expected RawStmt, found {}", other.name()))),
        })
        .collect()
}
