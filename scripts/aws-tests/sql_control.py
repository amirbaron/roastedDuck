#!/usr/bin/env python3
"""Differential AWS SQL controls using isolated profiles and no cloud account."""
import argparse
import hashlib
import gzip
import json
import os
from pathlib import Path
import subprocess


def quote(value):
    return str(value).replace("'", "''")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', type=Path, required=True)
    parser.add_argument('--oracle', type=Path, required=True)
    parser.add_argument('--aws-extension', type=Path, required=True)
    parser.add_argument('--httpfs-extension', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    config, credentials = args.out / 'config', args.out / 'credentials'
    config.write_text('[default]\nregion=us-east-1\n[profile named]\nregion=eu-west-1\n'
                      '[profile empty]\n[profile process]\nregion=us-east-2\n'
                      'credential_process=/usr/bin/printf \'{"Version":1,"AccessKeyId":"process-key",'
                      '"SecretAccessKey":"process-secret","SessionToken":"process-token"}\'\n')
    credentials.write_text('[default]\naws_access_key_id=default-key\naws_secret_access_key=default-secret\n'
                           '[named]\naws_access_key_id=named-key\naws_secret_access_key=named-secret\n')
    credentials.chmod(0o600)
    repository = args.out / 'repository'
    # The oracle's ordinary autoinstaller reads these signed, pinned artifacts.
    # No public repository or user's extension cache is touched.
    for path in [args.aws_extension, args.httpfs_extension]:
        data = path.read_bytes()
        # The final 256 bytes are the signature; metadata precedes it.
        platform = data[-320:-288].split(b'\0', 1)[0].decode()
        directory = repository / 'v1.5.6' / platform
        directory.mkdir(parents=True, exist_ok=True)
        (directory / path.name).write_bytes(data)
        (directory / (path.name + '.gz')).write_bytes(gzip.compress(data, mtime=0))
    env = {key: value for key, value in os.environ.items() if not key.startswith('AWS_')}
    env.update(AWS_CONFIG_FILE=str(config), AWS_SHARED_CREDENTIALS_FILE=str(credentials),
               AWS_PROFILE='default', AWS_REGION='us-east-1', AWS_EC2_METADATA_DISABLED='true',
               AWS_ENDPOINT_URL='http://127.0.0.1:1',
               AWS_ACCESS_KEY_ID='environment-key', AWS_SECRET_ACCESS_KEY='environment-secret',
               AWS_SESSION_TOKEN='environment-token')
    select = "SELECT name,type,provider,scope,secret_string FROM duckdb_secrets(redact=false);"
    cases = []
    for kind in ['s3', 'r2', 'gcs', 'aws']:
        cases.append((kind + '-env', f"CREATE SECRET fixture (TYPE {kind}, PROVIDER credential_chain, CHAIN 'env');\n{select}", {}, True))
    for name, params in [
        ('config-before-env', "CHAIN 'config;env', PROFILE 'named'"),
        ('env-before-config', "CHAIN 'env;config', PROFILE 'named'"),
        ('named-profile', "PROFILE 'named'"),
        ('process', "CHAIN 'process', PROFILE 'process'"),
        ('empty-profile', "CHAIN 'config', PROFILE 'empty', VALIDATION 'none'"),
        ('missing-profile-none', "PROFILE 'missing', VALIDATION 'none'"),
        ('overrides', "CHAIN 'env', KEY_ID 'override-key', SECRET 'override-secret', SESSION_TOKEN 'override-token', REGION 'override-region', ENDPOINT 'override-endpoint', URL_STYLE 'path', URL_COMPATIBILITY_MODE true, USE_SSL false"),
        ('refresh', "CHAIN 'env', REFRESH 'auto'"),
        ('refresh-inline-input', "CHAIN 'env', REFRESH 'auto', SECRET 'inline-secret', HTTP_PROXY_PASSWORD 'inline-proxy-password'"),
        ('refresh-profile-order', "PROFILE 'named', CHAIN 'env', REFRESH 'auto'"),
        ('refresh-collision-order', "REFRESH 'auto', REGION 'x', CHAIN 'env', PROFILE 'named'"),
    ]:
        cases.append((name, f"CREATE SECRET fixture (TYPE s3, PROVIDER credential_chain, {params});\n{select}", {}, True))
    cases.append(('refresh-input-redaction', "CREATE SECRET fixture(TYPE s3, PROVIDER credential_chain, CHAIN 'env', REFRESH 'auto', SECRET 'inline-secret', HTTP_PROXY_PASSWORD 'inline-proxy-password'); SELECT secret_string FROM duckdb_secrets();", {}, True))
    cases.extend([
        ('legacy', "CALL load_aws_credentials(); SELECT current_setting('s3_access_key_id'),current_setting('s3_region');", {}, True),
        ('legacy-profile', "CALL load_aws_credentials('named',redact_secret=false);", {}, True),
        ('legacy-region-priority', "CALL load_aws_credentials();", {'AWS_DEFAULT_REGION':'eu-central-1'}, True),
        ('legacy-no-region', "SET s3_region='unchanged'; CALL load_aws_credentials(set_region=false); SELECT current_setting('s3_region');", {}, True),
        ('legacy-null-profile', "CALL load_aws_credentials(NULL,set_region=false);", {}, True),
        ('region-warning', "CALL enable_logging(level='warning'); CREATE SECRET fixture (TYPE s3, PROVIDER credential_chain, PROFILE 'missing', VALIDATION 'none'); SELECT log_level,message FROM duckdb_logs WHERE message LIKE '%region%';", {'AWS_REGION': '', 'AWS_DEFAULT_REGION': ''}, True),
        ('unknown-provider', "CREATE SECRET fixture(TYPE s3,PROVIDER credential_chain,CHAIN 'nonsense');", {}, False),
        ('missing-profile', "CREATE SECRET fixture(TYPE s3,PROVIDER credential_chain,PROFILE 'missing');", {}, False),
        ('missing-after-env', "CREATE SECRET fixture(TYPE s3,PROVIDER credential_chain,CHAIN 'env;config',PROFILE 'missing');", {}, False),
        ('invalid-validation', "CREATE SECRET fixture(TYPE s3,PROVIDER credential_chain,VALIDATION 'nonsense');", {}, False),
        ('legacy-redaction', "SET allow_unredacted_secrets=false; CALL load_aws_credentials(redact_secret=false);", {}, False),
        ('type-without-httpfs', "CREATE SECRET fixture(TYPE s3,PROVIDER credential_chain,CHAIN 'env');", {}, False),
        ('legacy-without-httpfs', "CALL load_aws_credentials();", {}, False),
        ('autoload-secret', f"CREATE SECRET fixture(TYPE s3,PROVIDER credential_chain,CHAIN 'env'); {select}", {}, True),
        ('autoload-legacy', "CALL load_aws_credentials();", {}, True),
    ])
    records = []
    failures = []
    for name, sql, overrides, success in cases:
        outputs = {}
        for engine, cli in [('cpp', args.oracle), ('rust', args.cli)]:
            secret_path = args.out / engine / name / 'secrets'
            secret_path.parent.mkdir(parents=True)
            startup = f"SET secret_directory='{quote(secret_path)}';\n"
            if name.startswith('autoload-'):
                startup += f"SET extension_directory='{quote(secret_path.parent / 'extensions')}'; SET custom_extension_repository='{quote(repository)}'; SET autoinstall_known_extensions=true; SET autoload_known_extensions=true;\n"
            elif name.endswith('without-httpfs'):
                startup += 'SET autoload_known_extensions=false;\n'
                startup += (f"LOAD '{quote(args.aws_extension.resolve())}';\n" if engine == 'cpp' else 'LOAD aws;\n')
            elif engine == 'cpp':
                startup += f"LOAD '{quote(args.httpfs_extension.resolve())}'; LOAD '{quote(args.aws_extension.resolve())}';\n"
            else:
                startup += "SET extension_directory='/dev/null/aws-control'; SET custom_extension_repository='http://127.0.0.1:1'; INSTALL aws; LOAD httpfs; LOAD aws;\n"
            run = subprocess.run([str(cli.resolve()), '-batch', '-bail', '-csv', '-noheader', '-unredacted', '-init', '/dev/null', ':memory:'],
                                 input=startup + sql, env=dict(env, **overrides), text=True,
                                 capture_output=True, timeout=90)
            outputs[engine] = dict(exit=run.returncode, stdout=run.stdout, stderr=run.stderr)
        # Compare the actual SQL error, retaining raw shell rendering in the receipt.
        # A source-line/caret annotation is presentation, not part of the exception.
        def error_message(text):
            lines = text.strip().splitlines()
            for i, line in enumerate(lines):
                if line.startswith('LINE ') and ':' in line:
                    return '\n'.join(lines[:i]).strip()
            return '\n'.join(lines).strip()
        expected, actual = outputs['cpp'], outputs['rust']
        passed = ((expected['exit'] == 0) == success and (actual['exit'] == 0) == success
                  and expected['stdout'] == actual['stdout']
                  and error_message(expected['stderr']) == error_message(actual['stderr']))
        records.append(dict(case=name, sql=sql, expected_success=success, passed=passed, **outputs))
        if not passed:
            failures.append(name)
        (args.out / 'results.json').write_text(json.dumps(records, indent=2) + '\n')
    hashes = {}
    for name in ['cli', 'oracle', 'aws_extension', 'httpfs_extension']:
        with getattr(args, name).open('rb') as stream:
            hashes[name] = hashlib.file_digest(stream, 'sha256').hexdigest()
    report = dict(cases=len(records), passed=len(records) - len(failures), failures=failures,
                  sha256=hashes, scope='Isolated offline SQL differential; network/RDS/original SQL suites remain separate')
    (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report))
    if failures:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
