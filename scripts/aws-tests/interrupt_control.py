#!/usr/bin/env python3
"""Interrupt a real SQL credential lookup and verify its helper processes exit."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cli', type=Path, required=True)
    parser.add_argument('--api-probe', type=Path)
    parser.add_argument('--out', type=Path, required=True)
    args = parser.parse_args()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=False)
    helper, marker = args.out / 'slow.py', args.out / 'children.json'
    helper.write_text("import json,os,pathlib,subprocess,sys,time\np=subprocess.Popen(['sleep','30'])\npathlib.Path(sys.argv[1]).write_text(json.dumps({'helper':os.getpid(),'child':p.pid,'group':os.getpgrp()}))\ntime.sleep(30)\nprint(json.dumps({'Version':1,'AccessKeyId':'fixture-key','SecretAccessKey':'fixture-secret'}))\n")
    command = ' '.join(shlex.quote(str(p)) for p in [sys.executable, helper, marker])
    config, credentials = args.out / 'config', args.out / 'credentials'
    config.write_text(f'[profile slow]\nregion=us-east-1\ncredential_process={command}\n')
    credentials.write_text('')
    env = {k: v for k, v in os.environ.items() if not k.startswith('AWS_')}
    env.update(AWS_CONFIG_FILE=str(config), AWS_SHARED_CREDENTIALS_FILE=str(credentials),
               AWS_EC2_METADATA_DISABLED='true', AWS_REGION='us-east-1')
    sql = "SET allow_persistent_secrets=false;\nLOAD httpfs;\nLOAD aws;\nCREATE SECRET fixture(TYPE s3, PROVIDER credential_chain, CHAIN 'process', PROFILE 'slow');\n"
    command = ([str(args.api_probe.resolve()), str(marker)] if args.api_probe else
               [str(args.cli.resolve()), '-batch', '-bail', '-init', '/dev/null', ':memory:'])
    process = subprocess.Popen(command,
                               stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, env=env, start_new_session=True)
    children = {}
    try:
        if not args.api_probe:
            process.stdin.write(sql)
            process.stdin.flush()
        deadline = time.monotonic() + 15
        while not marker.exists() and time.monotonic() < deadline and process.poll() is None:
            time.sleep(0.02)
        if not marker.exists():
            raise RuntimeError('Credential helper did not start')
        children = json.loads(marker.read_text())
        if not args.api_probe:
            assert alive(children['helper']) and alive(children['child'])
            # Signal only the CLI, not its process group: the engine must cancel the helper.
            process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=10)
        deadline = time.monotonic() + 2
        while any(alive(children[k]) for k in ['helper', 'child']) and time.monotonic() < deadline:
            time.sleep(0.02)
        stopped = all(not alive(children[k]) for k in ['helper', 'child'])
        report = dict(sql=sql, mode='api' if args.api_probe else 'cli',
                      exit=process.returncode, stdout=stdout, stderr=stderr,
                      interrupt_diagnostic='INTERRUPT Error: Interrupted!' in stderr,
                      helper_processes_stopped=stopped)
        with (args.api_probe or args.cli).open('rb') as stream:
            report['executable_sha256'] = hashlib.file_digest(stream, 'sha256').hexdigest()
        # The shell suppresses diagnostics after Ctrl+C. The API probe separately
        # checks the actual ErrorKind, without relying on shell rendering.
        report['passed'] = stopped and ((process.returncode == 0 and stdout.strip() == 'interrupt-ok')
                                       if args.api_probe else process.returncode != 0)
        (args.out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps(report))
        assert report['passed']
    finally:
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
        # Failure cleanup is restricted to this fixture's recorded process group.
        if children and any(alive(children[k]) for k in ['helper', 'child']):
            try:
                os.killpg(children['group'], signal.SIGKILL)
            except ProcessLookupError:
                pass


if __name__ == '__main__':
    main()
