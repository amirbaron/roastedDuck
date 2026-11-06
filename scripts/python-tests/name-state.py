"""Alias argument/lifetime/state journeys beyond raw byte acceptance."""
import json
from pathlib import Path
import re
import sys

import duckdb
import _duckdb


class BrokenRepr:
    def __repr__(self):
        raise ValueError('user repr failed')


class SurrogateRepr:
    def __repr__(self):
        return 'x\ud800'


def failure(error):
    return {'error': type(error).__name__,
            'message': re.sub(r'(?<= at )0x[0-9a-fA-F]+', '0xADDR', str(error)),
            'context': type(error.__context__).__name__ if error.__context__ else None}


def capture(fn):
    try:
        relation = fn()
        return {'rows': relation.fetchall(), 'alias': relation.alias}
    except Exception as error:
        return failure(error)


con = duckdb.connect(config={'threads': 1})
results, generated = {}, {}
for scope, owner in [('connection', con), ('root', duckdb), ('native', _duckdb)]:
    for method in ['sql', 'query', 'from_query']:
        function = getattr(owner, method)
        connection = {} if scope == 'connection' else {'connection': con}
        key = f'{scope}/{method}'
        name = bytearray(b'before')
        relation = function('SELECT 42', alias=name, **connection)
        name[:] = b'after'
        results[key + '/copied'] = {'rows': relation.fetchall(), 'alias': relation.alias}
        results[key + '/params'] = capture(lambda: function('SELECT $1+1', alias=b'params', params=[41], **connection))
        statement = con.extract_statements('SELECT $1+1')[0]
        results[key + '/statement'] = capture(lambda: function(statement, alias=b'statement', params=[41], **connection))
        for label, args, kwargs in [
            ('positional_alias', ('SELECT 42', b'alias'), {}),
            ('duplicate_query', ('SELECT 42',), {'query': 'SELECT 43'}),
            ('unexpected', ('SELECT 42',), {'bad_keyword': True}),
            ('broken_repr', ('SELECT 42',), {'alias': BrokenRepr()}),
            ('surrogate_repr', ('SELECT 42',), {'alias': SurrogateRepr()}),
        ]:
            results[key + '/' + label] = capture(lambda: function(*args, **kwargs, **connection))
        relation = function('SELECT 42', **connection)
        generated[key] = relation.alias
        assert re.fullmatch('unnamed_relation_[0-9a-f]{16}', relation.alias)
        results[key + '/generated'] = {'rows': relation.fetchall(), 'pattern': True}
assert len(set(generated.values())) == len(generated)
base = con.sql('SELECT 42')
for label, value in [('broken_repr', BrokenRepr()), ('surrogate_repr', SurrogateRepr())]:
    results['relation/set_alias/' + label] = capture(lambda: base.set_alias(value))
uncached = con.sql('SELECT 43')
con.close()
for method in ['sql', 'query', 'from_query']:
    results[f'closed/{method}/valid_alias'] = capture(lambda: getattr(con, method)('SELECT 42', alias=b'alias'))
    results[f'closed/{method}/bad_alias'] = capture(lambda: getattr(con, method)('SELECT 42', alias=None))
results['closed/relation/valid_alias'] = capture(lambda: base.set_alias(b'alias'))
results['closed/relation/bad_alias'] = capture(lambda: base.set_alias(None))
results['closed/uncached_relation/bad_alias'] = capture(lambda: uncached.set_alias(None))
for label, relation in [('cached', base), ('uncached', uncached)]:
    try:
        results['closed/' + label + '_repr'] = {'repr': repr(relation)}
    except Exception as error:
        results['closed/' + label + '_repr'] = failure(error)
Path(sys.argv[1]).write_text(json.dumps({'outcomes': results, 'generated_aliases': generated,
    'note': 'Generated C++ aliases are random: compare their required shape and uniqueness, retain actual names separately.'}, indent=2) + '\n')
print(json.dumps({'cases': len(results)}))
