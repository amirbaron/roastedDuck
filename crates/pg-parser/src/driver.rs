//! The LALR(1) driver: a port of the deterministic part of bison 2.3's `yacc.c`
//! skeleton (`yyparse`), running the tables extracted into [`crate::tables`].
//!
//! DuckDB's grammar has no `error` productions and `yyerror` always throws, so the
//! first syntax error ends the parse; error recovery is not needed.

use roastedduck_common::Result;

use crate::actions::RULE_ACTIONS;
use crate::error::internal;
use crate::nodes::{List, Node, RawStmt};
use crate::scanner::Scanner;
use crate::semantic::{Reduction, RuleAction, SemValue, StackEntry};
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

/// The parser stack. Values left on it when a parse fails are torn down without
/// recursion, since they may hold arbitrarily deep expression trees.
struct Stack<'a>(Vec<StackEntry<'a>>);

impl Stack<'_> {
    fn new() -> Self {
        let mut entries = Vec::with_capacity(INITIAL_STACK_CAPACITY);
        // The bottom value/location slot is never read (bison "wastes" it).
        entries.push(StackEntry {
            state: 0,
            location: 0,
            value: SemValue::None,
        });
        Self(entries)
    }

    fn top(&self) -> usize {
        self.0.last().map_or(0, |entry| entry.state)
    }
}

impl Drop for Stack<'_> {
    fn drop(&mut self) {
        self.0.drain(..).for_each(|entry| entry.value.drop_tree());
    }
}

/// `yygoto`: the state after reducing to nonterminal `symbol` on top of `state`.
fn goto(symbol: usize, state: usize) -> Result<usize> {
    let lhs = symbol - YYNTOKENS;
    match packed(YYPGOTO[lhs], state) {
        Some(target) => Ok(usize::from(target.unsigned_abs())),
        None => usize::try_from(YYDEFGOTO[lhs]).map_err(|_| internal("bad goto")),
    }
}

/// Runs the parser over the whole input and returns the statement list (the value
/// of `stmtblock`).
pub(crate) fn parse(scanner: &mut Scanner<'_>) -> Result<List> {
    let mut stack = Stack::new();
    // The lookahead token: symbol, value and location (`yychar`, `yylval`, `yylloc`).
    let mut lookahead: Option<(usize, SemValue<'_>, i32)> = None;

    loop {
        if stack.0.len() >= YYMAXDEPTH {
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
                stack.0.push(StackEntry {
                    state: target,
                    location,
                    value,
                });
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
        let len = usize::from(YYR2[rule]);
        let base = stack.0.len() - len;
        let lhs = usize::from(YYR1[rule]);
        let action = RULE_ACTIONS[rule];
        if len == 1 && matches!(action, RuleAction::Default) {
            // `$$ = $1` and `@$ = @1`: the value and location stay where they are.
            let below = stack.0[base - 1].state;
            stack.0[base].state = goto(lhs, below)?;
            continue;
        }
        let mut rhs = Reduction::new(&mut stack.0[base..], scanner, rule);
        let value = match action {
            RuleAction::Default => rhs.take_raw(1),
            RuleAction::Forward(k) => rhs.take_raw(k),
            RuleAction::Call(action, alt) => action(&mut rhs, alt)?,
        };
        let location = rhs.location;
        stack.0.truncate(base);
        let state = goto(lhs, stack.top())?;
        stack.0.push(StackEntry {
            state,
            location,
            value,
        });
    }

    // Accepted: the value of `stmtblock` is on top of the stack.
    match stack.0.pop().map(|entry| entry.value) {
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
