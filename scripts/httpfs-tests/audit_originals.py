#!/usr/bin/env python3
"""Aggregate retained receipts without counting upstream-disabled SQL as coverage."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def mode_audit(text):
    depth = 0
    active = disabled = setup = 0
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if line == 'mode skip':
            depth += 1
        elif line == 'mode unskip':
            depth -= 1
        elif line.startswith(('statement ', 'query ')):
            if depth:
                disabled += 1
            else:
                active += 1
                if line == 'statement ok':
                    sql = []
                    for following in lines[index + 1:]:
                        if not following.strip() or following == '----':
                            break
                        if not following.lstrip().startswith('#'):
                            sql.append(following.strip())
                    normalized = ' '.join(' '.join(sql).lower().rstrip(';').split())
                    if normalized in ['pragma enable_verification', 'pragma disable_verification']:
                        setup += 1
    return dict(active_sql_records=active, disabled_sql_records=disabled,
                verification_setup_records=setup,
                setup_only=bool(disabled and active and active == setup))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--receipt', type=Path, action='append', required=True,
                        help='Chronological order; later per-file verdicts replace earlier ones')
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    manifest = json.loads((ROOT / 'extensions/httpfs/upstream.json').read_text())
    files = {}
    for name, expected in manifest['sql_tests'].items():
        content = (args.source / name).read_bytes()
        assert hashlib.sha256(content).hexdigest() == expected
        files[name] = dict(sha256=expected, modes=mode_audit(content.decode()),
                          cpp=dict(status='not_run'), rust=dict(status='not_run'))
    for receipt in args.receipt:
        data = json.loads(receipt.read_text())
        for engine in ['cpp', 'rust']:
            for row in data.get(engine, {}).get('rows', []):
                files[row['file']][engine] = dict(row, receipt=str(receipt.resolve()))
    disabled = [name for name, entry in files.items()
                if entry['modes']['disabled_sql_records'] and not entry['modes']['active_sql_records']]
    setup_only = [name for name, entry in files.items() if entry['modes']['setup_only']]
    excluded = set(disabled + setup_only)
    both = [name for name, entry in files.items() if name not in excluded
            and all(entry[engine]['status'] == 'pass' for engine in ['cpp', 'rust'])]
    report = dict(scope='Retained per-file results, not a frozen full sweep. Raw SLT statuses remain unchanged.',
                  raw_counts={engine: dict(Counter(entry[engine]['status'] for entry in files.values()))
                              for engine in ['cpp', 'rust']},
                  upstream_disabled_sql=disabled, upstream_setup_only=setup_only, exercised_pass_both=both,
                  unresolved_active=[name for name in files if name not in both and name not in excluded],
                  files=files)
    args.out.write_text(json.dumps(report, indent=2) + '\n')
    print(f'{len(both)} exercised originals pass both engines; {len(disabled)} upstream-disabled; {len(setup_only)} setup-only; '
          f'{len(report["unresolved_active"])} active originals remain unresolved')


if __name__ == '__main__':
    main()
