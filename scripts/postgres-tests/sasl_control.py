#!/usr/bin/env python3
"""SASL mechanism selection against an owned protocol fixture.

A real server always offers SCRAM-SHA-256 next to SCRAM-SHA-256-PLUS over TLS, so
the mechanism lists a client must refuse need a fixture. This one speaks only the
start of the protocol: TLS, the startup packet, an AuthenticationSASL message with
the scenario's mechanisms, then it records the client's first SASL message (its
mechanism and GS2 header) and ends the attempt with a FATAL error. It never
checks a password and is not a PostgreSQL server.
"""
import argparse
from datetime import datetime, timedelta, timezone
import hashlib
import ipaddress
import json
from pathlib import Path
import re
import socket
import ssl
import struct
import subprocess
import threading

SCENARIOS = [
    ('plus-only-disable', ['SCRAM-SHA-256-PLUS'], 'disable'),
    ('plus-only-prefer', ['SCRAM-SHA-256-PLUS'], 'prefer'),
    ('plus-only-require', ['SCRAM-SHA-256-PLUS'], 'require'),
    ('scram-only-disable', ['SCRAM-SHA-256'], 'disable'),
    ('scram-only-prefer', ['SCRAM-SHA-256'], 'prefer'),
    ('scram-only-require', ['SCRAM-SHA-256'], 'require'),
    ('both-disable', ['SCRAM-SHA-256-PLUS', 'SCRAM-SHA-256'], 'disable'),
    ('both-prefer', ['SCRAM-SHA-256-PLUS', 'SCRAM-SHA-256'], 'prefer'),
    ('both-reversed-prefer', ['SCRAM-SHA-256', 'SCRAM-SHA-256-PLUS'], 'prefer'),
    ('both-require', ['SCRAM-SHA-256-PLUS', 'SCRAM-SHA-256'], 'require'),
    ('unknown-only', ['OWNED-FIXTURE-MECHANISM'], 'prefer'),
]


def certificate(directory):
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import rsa
    from cryptography.x509.oid import NameOID
    key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'localhost')])
    now = datetime.now(timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key())
            .serial_number(x509.random_serial_number()).not_valid_before(now - timedelta(days=1))
            .not_valid_after(now + timedelta(days=2))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName('localhost'),
                           x509.IPAddress(ipaddress.ip_address('127.0.0.1'))]), critical=False)
            .sign(key, hashes.SHA256()))
    (directory / 'server.crt').write_bytes(cert.public_bytes(serialization.Encoding.PEM))
    (directory / 'server.key').write_bytes(key.private_bytes(serialization.Encoding.PEM,
                                           serialization.PrivateFormat.PKCS8, serialization.NoEncryption()))
    (directory / 'server.key').chmod(0o600)


def read_exact(connection, count):
    data = b''
    while len(data) < count:
        chunk = connection.recv(count - len(data))
        if not chunk:
            raise EOFError
        data += chunk
    return data


class Fixture:
    def __init__(self, directory, mechanisms):
        self.mechanisms = mechanisms
        self.observed = []
        self.lock = threading.Lock()
        self.context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.context.load_cert_chain(directory / 'server.crt', directory / 'server.key')
        self.listener = socket.create_server(('127.0.0.1', 0))
        self.port = self.listener.getsockname()[1]
        self.thread = threading.Thread(target=self.serve, daemon=True)
        self.thread.start()

    def serve(self):
        while True:
            try:
                connection, _ = self.listener.accept()
            except OSError:
                return
            threading.Thread(target=self.attempt, args=(connection,), daemon=True).start()

    def attempt(self, connection):
        record = dict(tls=False, startup=False, sasl=None)
        try:
            connection.settimeout(10)
            while True:
                length, code = struct.unpack('!II', read_exact(connection, 8))
                if code == 80877104:  # GSSENCRequest
                    connection.sendall(b'N')
                    continue
                break
            if length != 8 or code != 80877103:  # SSLRequest
                return
            connection.sendall(b'S')
            connection = self.context.wrap_socket(connection, server_side=True)
            record['tls'] = True
            length, = struct.unpack('!I', read_exact(connection, 4))
            read_exact(connection, length - 4)
            record['startup'] = True
            body = struct.pack('!I', 10) + b''.join(name.encode() + b'\0' for name in self.mechanisms) + b'\0'
            connection.sendall(b'R' + struct.pack('!I', len(body) + 4) + body)
            kind = read_exact(connection, 1)
            length, = struct.unpack('!I', read_exact(connection, 4))
            payload = read_exact(connection, length - 4)
            if kind == b'p':
                mechanism, rest = payload.split(b'\0', 1)
                initial = rest[4:]
                header = initial.split(b',', 1)[0].decode(errors='replace')
                record['sasl'] = dict(mechanism=mechanism.decode(), gs2=header)
            else:
                record['sasl'] = dict(message=kind.decode(errors='replace'))
            fields = b'SFATAL\0VFATAL\0C28000\0Mowned fixture ends authentication here\0\0'
            connection.sendall(b'E' + struct.pack('!I', len(fields) + 4) + fields)
        except (EOFError, OSError, ssl.SSLError):
            pass
        finally:
            with self.lock:
                self.observed.append(record)
            try:
                connection.close()
            except OSError:
                pass

    def close(self):
        self.listener.close()
        self.thread.join()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['cli', 'oracle', 'extension', 'out']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    out = args.out.resolve()
    out.mkdir(parents=True, exist_ok=False)
    certificate(out)
    cases = []
    for label, mechanisms, binding in SCENARIOS:
        engines = {}
        for engine in ['cpp', 'rust']:
            directory = out / f'{label}-{engine}'
            directory.mkdir()
            (directory / 'secrets').mkdir()
            fixture = Fixture(out, mechanisms)
            try:
                load = "LOAD '" + str(args.extension.resolve()).replace("'", "''") + "';" if engine == 'cpp' else 'LOAD postgres;'
                sql = ("SET secret_directory='" + str(directory / 'secrets').replace("'", "''") + "';" + load +
                       f"ATTACH 'host=127.0.0.1 port={fixture.port} dbname=fixture user=fixture password=owned-fixture "
                       f"sslmode=require channel_binding={binding} connect_timeout=10' AS p (TYPE postgres);")
                binary = args.oracle if engine == 'cpp' else args.cli
                run = subprocess.run([str(binary.resolve()), '-batch', '-bail', '-csv', '-noheader', '-init', '/dev/null', ':memory:'],
                                     input=sql, text=True, capture_output=True, timeout=60, cwd=directory)
            finally:
                fixture.close()
            (directory / 'query.sql').write_text(sql)
            (directory / 'stderr').write_text(run.stderr)
            engines[engine] = dict(exit=run.returncode,
                                   error=re.sub(r'port=\d+', 'port=<port>', run.stderr.strip()),
                                   attempts=fixture.observed)
        # The gate is what the client put on the wire. The reference connects twice per
        # failed ATTACH and words connection failures as libpq does; roastedDuck connects
        # once and uses its own client's wording. Both are recorded, not required equal.
        selection = {engine: sorted({json.dumps(attempt, sort_keys=True) for attempt in run['attempts']})
                     for engine, run in engines.items()}
        reason = {engine: run['error'].rsplit(' failed: ', 1)[-1].replace('authentication error: ', '').replace('FATAL:  ', 'FATAL: ')
                  for engine, run in engines.items()}
        record = dict(name=label, mechanisms=mechanisms, channel_binding=binding, **engines,
                      same_selection=selection['cpp'] == selection['rust'],
                      same_reason=reason['cpp'].endswith(reason['rust'].removeprefix('IO Error: ')),
                      same_error_text=engines['cpp']['error'] == engines['rust']['error'],
                      same_attempt_count=len(engines['cpp']['attempts']) == len(engines['rust']['attempts']))
        record['passed'] = (record['same_selection'] and record['same_reason']
                            and engines['cpp']['exit'] == engines['rust']['exit'])
        cases.append(record)
        print(json.dumps({key: record[key] for key in ['name', 'passed', 'same_selection', 'same_reason', 'same_error_text', 'same_attempt_count']}), flush=True)
        (out / 'progress.json').write_text(json.dumps(cases, indent=2) + '\n')
    report = dict(cases=cases, scope='Owned protocol fixture for SASL mechanism selection; not a PostgreSQL server. '
                  'Passing means the same wire selection and failure reason; full error text and attempt counts are reported separately',
                  sha256={name: hashlib.sha256(getattr(args, name).read_bytes()).hexdigest() for name in ['cli', 'oracle', 'extension']})
    (out / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
    if not all(case['passed'] for case in cases):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
