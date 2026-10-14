//! The LALR(1) driver: a port of the deterministic part of bison 2.3's `yacc.c`
//! skeleton (`yyparse`), running the tables extracted into [`crate::tables`].
//!
//! DuckDB's grammar has no `error` productions and `yyerror` always throws, so the
//! first syntax error ends the parse; error recovery is not needed.

use rustdb_common::Result;

use crate::actions::RULE_ACTIONS;
use crate::error::internal;
use crate::nodes::{List, Node, RawStmt};
use crate::scanner::Scanner;
use crate::semantic::{Reduction, RuleAction, SemValue};
use crate::tables::{
    YYCHECK, YYDEFACT, YYDEFGOTO, YYFINAL, YYLAST, YYMAXDEPTH, YYMAXUTOK, YYNTOKENS, YYPACT, YYPACT_NINF,
    YYPGOTO, YYR1, YYR2, YYTABLE, YYTABLE_NINF, YYTRANSLATE, YYUNDEFTOK,
};

/// Initial capacity of the parser stacks. Bison starts with `YYINITDEPTH` (1000)
/// entries on the C stack; heap stacks that grow on demand are cheaper here,
/// since typical statements stay shallow.
const INITIAL_STACK_CAPACITY: usize = 64;

/// `YYTRANSLATE`: maps a token code to a grammar symbol number.
fn translate(code: u16) -> usize {
    let code = usize::from(code);
    if code <= YYMAXUTOK {
        usize::from(YYTRANSLATE[code])
    } else {
        YYUNDEFTOK
    }
}

/// Looks up `yytable[base + key]` if `yycheck` confirms the entry belongs to `key`.
fn packed(base: i32, key: usize) -> Option<i16> {
    let index = usize::try_from(i64::from(base) + key as i64)
        .ok()
        .filter(|&i| i <= YYLAST)?;
    (usize::try_from(YYCHECK[index]).ok() == Some(key)).then(|| YYTABLE[index])
}

/// What the action table says to do in a state for the lookahead symbol.
enum Step {
    Shift(usize),
    Reduce(usize),
    Accept,
    Error,
}

/// The parser stacks: states, semantic values and locations, kept the same height.
///
/// Values left on the stack when a parse fails are torn down without recursion,
/// since they may hold arbitrarily deep expression trees.
struct Stack<'a> {
    states: Vec<usize>,
    values: Vec<SemValue<'a>>,
    locations: Vec<i32>,
}

impl<'a> Stack<'a> {
    fn new() -> Self {
        let mut stack = Self {
            states: Vec::with_capacity(INITIAL_STACK_CAPACITY),
            values: Vec::with_capacity(INITIAL_STACK_CAPACITY),
            locations: Vec::with_capacity(INITIAL_STACK_CAPACITY),
        };
        // The bottom value/location slots are never read (bison "wastes" them).
        stack.push(0, SemValue::None, 0);
        stack
    }

    fn top(&self) -> usize {
        self.states.last().copied().unwrap_or(0)
    }

    fn push(&mut self, state: usize, value: SemValue<'a>, location: i32) {
        self.states.push(state);
        self.values.push(value);
        self.locations.push(location);
    }
}

impl Drop for Stack<'_> {
    fn drop(&mut self) {
        self.values.drain(..).for_each(SemValue::drop_tree);
    }
}

/// Runs the parser over the whole input and returns the statement list (the value
/// of `stmtblock`).
pub(crate) fn parse(scanner: &mut Scanner<'_>) -> Result<List> {
    let mut stack = Stack::new();
    // The lookahead token: symbol, value and location (`yychar`, `yylval`, `yylloc`).
    let mut lookahead: Option<(usize, SemValue<'_>, i32)> = None;

    loop {
        if stack.states.len() >= YYMAXDEPTH {
            return Err(scanner.syntax_error("memory exhausted"));
        }
        let state = stack.top();

        // yybackup: consult the action table if the state needs a lookahead.
        let pact = YYPACT[state];
        let step = if pact == YYPACT_NINF {
            None
        } else {
            let symbol = if let Some((symbol, _, _)) = &lookahead {
                *symbol
            } else {
                let token = scanner.next_token()?;
                let symbol = if token.code == 0 { 0 } else { translate(token.code) };
                let location = i32::try_from(token.loc).map_err(|_| internal("query too long"))?;
                lookahead = Some((symbol, token.value, location));
                symbol
            };
            packed(pact, symbol).map(|action| match action {
                0 => Step::Error,
                a if i32::from(a) == YYTABLE_NINF => Step::Error,
                a if a < 0 => Step::Reduce(usize::from(a.unsigned_abs())),
                a if usize::from(a.unsigned_abs()) == YYFINAL => Step::Accept,
                a => Step::Shift(usize::from(a.unsigned_abs())),
            })
        };

        // yydefault: without an explicit action, take the state's default reduction.
        let rule = match step {
            Some(Step::Shift(target)) => {
                let (_, value, location) = lookahead
                    .take()
                    .ok_or_else(|| internal("shift without lookahead"))?;
                stack.push(target, value, location);
                continue;
            }
            Some(Step::Accept) => break,
            Some(Step::Reduce(rule)) => rule,
            Some(Step::Error) => return Err(scanner.syntax_error("syntax error")),
            None => match usize::from(YYDEFACT[state]) {
                0 => return Err(scanner.syntax_error("syntax error")),
                rule => rule,
            },
        };

        // yyreduce: run the rule's action on the top `len` entries.
        let base = stack.states.len() - usize::from(YYR2[rule]);
        let (value, location) = {
            let mut rhs = Reduction::new(&mut stack.values[base..], &stack.locations[base..], scanner, rule);
            let value = match RULE_ACTIONS[rule] {
                RuleAction::Default => rhs.take_raw(1),
                RuleAction::Forward(k) => rhs.take_raw(k),
                RuleAction::Call(action, alt) => action(&mut rhs, alt)?,
            };
            (value, rhs.location)
        };
        stack.states.truncate(base);
        stack.values.truncate(base);
        stack.locations.truncate(base);

        // Goto the state for the reduced nonterminal.
        let lhs = usize::from(YYR1[rule]) - YYNTOKENS;
        let top = stack.top();
        let next = match packed(YYPGOTO[lhs], top) {
            Some(target) => usize::from(target.unsigned_abs()),
            None => usize::try_from(YYDEFGOTO[lhs]).map_err(|_| internal("bad goto"))?,
        };
        stack.push(next, value, location);
    }

    // Accepted: the value of `stmtblock` is on top of the stack.
    match stack.values.pop() {
        Some(SemValue::List(list)) => Ok(list),
        _ => Err(internal("parser accepted without a statement list")),
    }
}

/// Unwraps the raw statements built by `stmtmulti`.
pub(crate) fn into_raw_statements(list: List) -> Result<Vec<RawStmt>> {
    list.into_iter()
        .map(|node| match node {
            Node::RawStmt(stmt) => Ok(*stmt),
            other => Err(internal(format!("expected RawStmt, found {}", other.name()))),
        })
        .collect()
}
