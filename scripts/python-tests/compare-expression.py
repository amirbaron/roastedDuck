"""Public native expression construction, immutability, SQL names and evaluation."""
import json
from pathlib import Path
import sys
import duckdb
from duckdb import ColumnExpression as C, ConstantExpression as V

outcomes = {}


def capture(name, action):
    try:
        outcomes[name] = {'value': action()}
    except Exception as error:
        outcomes[name] = {'error': type(error).__name__, 'message': str(error)}


a = C('a')
expressions = {
    'column': a, 'qualified': C('t.a'), 'literal_parts': C('t.x', 'a.b'),
    'null': V(None), 'unicode': V('雪'), 'add': a + 2, 'rsub': 2 - a,
    'mul': a * 2, 'div': a / 2, 'floor': a // 2, 'mod': a % 2,
    'power': a ** 2, 'rpower': 2 ** a, 'negate': -a,
    'compare': a >= 2, 'conjunction': (a > 0) & (a < 3),
    'disjunction': (a == 1) | (a == 3), 'not': ~(a == 1),
    'isnull': a.isnull(), 'isnotnull': a.isnotnull(),
    'between': a.between(1, 3), 'isin': a.isin(1, 3), 'isnotin': a.isnotin(1, 3),
    'alias': (a + 1).alias('answer'), 'cast': a.cast('VARCHAR'),
    'builtin_cast': a.cast(int), 'collate': C('b').collate('nocase'),
    'case': duckdb.CaseExpression(a == 1, V('one')).when(a == 2, V('two')).otherwise(V('other')),
    'coalesce': duckdb.CoalesceOperator(a, V(42)),
    'function': duckdb.FunctionExpression('upper', C('b')),
    'star': duckdb.StarExpression(exclude=['a']), 'default': duckdb.DefaultExpression(),
    'sql': duckdb.SQLExpression('a + 2'),
    'lambda': duckdb.LambdaExpression('x', C('x') + 1),
}
for name, expression in expressions.items():
    capture('render_' + name, lambda expression=expression: [str(expression), repr(expression), expression.get_name()])
capture('immutable', lambda: [str(a), str(a.alias('x')), a.get_name(), a.alias('x').get_name()])
capture('cast_keyword', lambda: [str(a.cast(type='VARCHAR')),a.cast(type='VARCHAR').get_name()])
for name, action in {
    'isin_empty': lambda: a.isin(),
    'case_invalid': lambda: a.when(True, 1),
    'lambda_invalid': lambda: duckdb.LambdaExpression(42, a),
    'sql_many': lambda: duckdb.SQLExpression('1,2'),
    'sql_invalid': lambda: duckdb.SQLExpression('1 +'),
    'star_invalid': lambda: duckdb.StarExpression(exclude=[V(42)]),
}.items():
    capture(name, action)
with duckdb.connect() as con:
    relation = con.sql("SELECT * FROM (VALUES (1,'b'),(2,'a'),(3,NULL),(NULL,'c')) t(a,b)")
    for name in ['add', 'rsub', 'mul', 'div', 'floor', 'mod', 'power', 'rpower',
                 'compare', 'conjunction', 'disjunction', 'not', 'isnull', 'between',
                 'isin', 'isnotin', 'case', 'coalesce', 'function']:
        capture('evaluate_' + name, lambda name=name: relation.select(expressions[name]).fetchall())
    capture('sort', lambda: relation.sort(a.desc().nulls_first()).select(a).fetchall())
    capture('filter', lambda: relation.filter(a > 1).select(a, C('b')).fetchall())
    capture('aggregate', lambda: relation.aggregate([duckdb.FunctionExpression('sum', a)]).fetchall())
Path(sys.argv[1]).write_text(json.dumps({'version': duckdb.__version__, 'outcomes': outcomes}, indent=2) + '\n')
print(json.dumps({'outcomes': len(outcomes)}))
