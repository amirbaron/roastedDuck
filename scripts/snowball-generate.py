#!/usr/bin/env python3
"""Regenerate Rust Snowball from a separately built, pinned development generator.

No Cargo build invokes this script or compiles/links C code. Usage:
  python3 scripts/snowball-generate.py --source /ssd/snowball --duckdb /path/duckdb-1.5.6
The source tree must be snowballstem/snowball@896281c2d89c9347f83426d37ad9b36fc4742d5b.
The executable `source/snowball` must already exist (one-time development tooling).
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

PIN = "896281c2d89c9347f83426d37ad9b36fc4742d5b"
ROOT = Path(__file__).resolve().parent.parent


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def c_tokens(text):
    # Preserve whitespace inside literals, ignoring only layout and C comments.
    token = re.compile(r'/\*.*?\*/|//[^\n]*|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'|[A-Za-z_][A-Za-z_0-9]*|[0-9]+|[^\s]', re.S)
    return [x for x in token.findall(text) if not x.startswith(('/*', '//'))]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--duckdb', type=Path, required=True)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    source, ref = args.source.resolve(), args.duckdb.resolve() / 'third_party/snowball'
    dest = ROOT / 'crates/snowball'
    aliases = re.findall(r'\{"(.*?)", ENC_UTF_8, (\w+)_UTF_8_create_env', (ref / 'libstemmer/modules.h').read_text())
    names = sorted({name for _, name in aliases})
    assert len(names) == 27 and len(aliases) == 86
    manifest = {'upstream': 'https://github.com/snowballstem/snowball', 'commit': PIN,
                'duckdb': '1.5.6', 'modules_sha256': digest(ref / 'libstemmer/modules.h'),
                'compiler_sources': {str(p.relative_to(source)): digest(p) for p in sorted((source / 'compiler').glob('*')) if p.suffix in ('.c', '.h')},
                'algorithms': [], 'aliases': aliases}
    with tempfile.TemporaryDirectory(prefix='rust-generation-', dir=source.parent) as tmp:
        tmp = Path(tmp)
        for name in names:
            definition = source / 'algorithms' / f'{name}.sbl'
            generated = tmp / f'stem_UTF_8_{name}'
            subprocess.run([str(source / 'snowball'), str(definition), '-o', str(generated), '-eprefix', f'{name}_UTF_8_', '-r', '../runtime', '-u'], check=True)
            original = ref / f'src_c/stem_UTF_8_{name}.cpp'
            assert c_tokens(generated.with_suffix('.c').read_text()) == c_tokens(original.read_text()), f'{name}: generated C differs from DuckDB!'
            rust = tmp / f'{name}_stemmer'
            subprocess.run([str(source / 'snowball'), str(definition), '-rust', '-o', str(rust)], check=True)
            text = rust.with_suffix('.rs').read_text()
            text = text.replace('use snowball::', 'use crate::runtime::')
            # Byte carriers preserve the C runtime semantics, including embedded NUL.
            text = text.replace('String', 'Vec<u8>').replace('Vec<u8>::new()', 'Vec::new()')
            text = text.replace('env.current.chars().count() as i32', 'env.len_utf8()')
            text = text.replace('#![allow(non_upper_case_globals)]', '#![allow(clippy::all, clippy::pedantic)]\n#![allow(non_upper_case_globals)]')
            target = dest / 'src/algorithms' / f'{name}_stemmer.rs'
            # rustfmt keeps checked-in generated sources compatible with workspace formatting.
            formatted = subprocess.run(['rustfmt', '--edition', '2024', '--emit', 'stdout'], input=text, text=True, capture_output=True, check=True).stdout
            if args.check:
                assert target.read_text() == formatted, f'{target} is stale'
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(formatted)
            manifest['algorithms'].append({'name': name, 'definition_sha256': digest(definition), 'duckdb_c_sha256': digest(original), 'generated_c_matches': True, 'rust_sha256': hashlib.sha256(formatted.encode()).hexdigest()})
    document = json.dumps(manifest, indent=2) + '\n'
    if args.check:
        assert (dest / 'provenance.json').read_text() == document
    else:
        (dest / 'provenance.json').write_text(document)
        (dest / 'LICENSE').write_bytes((source / 'COPYING').read_bytes())
    print(f'{len(names)} generated C algorithms match DuckDB; Rust sources ' + ('verified' if args.check else 'written'))


if __name__ == '__main__':
    main()
