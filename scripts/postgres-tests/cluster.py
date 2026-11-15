"""Owned PostgreSQL cluster for connector tests; never connects to a user's server."""
from datetime import datetime, timedelta, timezone
import ipaddress
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess

PASSWORD = 'roastedduck-disposable-fixture-only'
ADMIN = 'roastedduck_fixture_admin'
USER = 'postgres'
TEMPLATE = 'roastedduck_fixture_template'
DATABASE = 'postgresscanner'


def quote_identifier(value):
    return '"' + value.replace('"', '""') + '"'


class Cluster:
    def __init__(self, out, fixed_port=None, password=PASSWORD, checkpoint_role=False):
        self.out = Path(out).resolve()
        self.started = False
        self.commands = []
        self.tools = {}
        self.fixed_port = fixed_port
        self.password = password
        self.checkpoint_role = checkpoint_role
        for name in ['initdb', 'pg_ctl', 'psql']:
            path = shutil.which(name)
            if path is None:
                raise RuntimeError(f'{name} is required for disposable PostgreSQL tests')
            self.tools[name] = path

    def __enter__(self):
        if self.fixed_port is not None:
            # Some unchanged originals explicitly use localhost:5432. Never
            # connect them to an existing local service or stop that service.
            for family,address in [(socket.AF_INET,'127.0.0.1'),(socket.AF_INET6,'::1')]:
                with socket.socket(family) as probe:
                    probe.bind((address,self.fixed_port))
        self.out.mkdir(parents=True, exist_ok=False)
        self.data = self.out / 'data'
        self.tmp = self.out / 'tmp'
        self.tmp.mkdir()
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(('PG','AWS_','AZURE_','RDS_','POSTGRES_TEST_','DUCKDB_AWS_','DUCKDB_RDS_'))}
        for name in ['pgpass', 'pg_service.conf']:
            path = self.out / name
            path.write_text('')
            path.chmod(0o600)
        env.update(TMPDIR=str(self.tmp), PGPASSFILE=str(self.out/'pgpass'),
                   PGSERVICEFILE=str(self.out/'pg_service.conf'), PGPASSWORD=self.password)
        password = self.out/'init-password'
        password.write_text(self.password+'\n')
        password.chmod(0o600)
        with (self.out/'initdb.log').open('w') as log:
            subprocess.run([self.tools['initdb'], '-D', str(self.data), '-U', ADMIN,
                            '--no-locale', '-E', 'UTF8', '--auth-local=trust',
                            '--auth-host=scram-sha-256', '--pwfile', str(password)],
                           env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=60)
        password.unlink()
        self._certificate()
        with socket.socket() as sock:
            sock.bind(('127.0.0.1', 0))
            self.port = self.fixed_port or sock.getsockname()[1]
        addresses='127.0.0.1,::1' if self.fixed_port is not None else '127.0.0.1'
        with (self.data/'postgresql.conf').open('a') as config:
            config.write(f"\nlisten_addresses='{addresses}'\nport={self.port}\nunix_socket_directories=''\n"
                         "max_connections=160\nshared_buffers='16MB'\nssl=on\n"
                         "ssl_cert_file='server.crt'\nssl_key_file='server.key'\n"
                         "log_min_messages=warning\nlog_error_verbosity=terse\n")
        self.environment = dict(env, PGHOST='127.0.0.1', PGPORT=str(self.port),
                                PGUSER=USER, PGDATABASE=DATABASE,
                                PGSSLROOTCERT=str(self.data/'root.crt'), PGSSLMODE='verify-full')
        try:
            subprocess.run([self.tools['pg_ctl'], '-D', str(self.data), '-l', str(self.out/'postgres.log'),
                            '-w', 'start'], env=env, check=True, capture_output=True, text=True, timeout=60)
            self.started = True
            if self.fixed_port is not None:
                pid=(self.data/'postmaster.pid').read_text().splitlines()[0]
                owned=subprocess.check_output(['lsof','-nP','-a','-p',pid,'-iTCP:'+str(self.port),'-sTCP:LISTEN','-Fn'],text=True)
                if f'n127.0.0.1:{self.port}' not in owned or f'n[::1]:{self.port}' not in owned:
                    raise RuntimeError('Owned fixture must hold both localhost listeners before explicit-URI tests run')
            # Administrative privileges are used only for fixture provisioning.
            # Tests run as a database owner without superuser or role-management rights.
            quoted_password=self.password.replace("'","''")
            self.sql(f"CREATE ROLE {quote_identifier(USER)} LOGIN PASSWORD '{quoted_password}' NOSUPERUSER NOCREATEDB NOCREATEROLE;", admin=True)
            if self.checkpoint_role:
                self.sql(f'GRANT pg_checkpoint TO {quote_identifier(USER)};',admin=True)
            self.sql(f"ALTER DATABASE postgres OWNER TO {quote_identifier(USER)};", admin=True)
            self.sql(f"CREATE DATABASE {TEMPLATE} OWNER {quote_identifier(USER)};", admin=True)
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def _certificate(self):
        from cryptography import x509
        from cryptography.hazmat.primitives import hashes, serialization
        from cryptography.hazmat.primitives.asymmetric import rsa
        from cryptography.x509.oid import NameOID
        key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        root_key = rsa.generate_private_key(public_exponent=65537, key_size=2048)
        name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'localhost')])
        root_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, 'Disposable PostgreSQL fixture CA')])
        now = datetime.now(timezone.utc)
        root = (x509.CertificateBuilder().subject_name(root_name).issuer_name(root_name).public_key(root_key.public_key())
                .serial_number(x509.random_serial_number()).not_valid_before(now-timedelta(days=1))
                .not_valid_after(now+timedelta(days=2))
                .add_extension(x509.BasicConstraints(ca=True,path_length=0),critical=True)
                .sign(root_key,hashes.SHA256()))
        (self.data/'root.crt').write_bytes(root.public_bytes(serialization.Encoding.PEM))
        cert = (x509.CertificateBuilder().subject_name(name).issuer_name(root_name).public_key(key.public_key())
                .serial_number(x509.random_serial_number()).not_valid_before(now-timedelta(days=1))
                .not_valid_after(now+timedelta(days=2))
                .add_extension(x509.BasicConstraints(ca=False,path_length=None),critical=True)
                .add_extension(x509.SubjectAlternativeName([x509.DNSName('localhost'),
                                x509.IPAddress(ipaddress.ip_address('127.0.0.1'))]),critical=False)
                .sign(root_key,hashes.SHA256()))
        (self.data/'server.crt').write_bytes(cert.public_bytes(serialization.Encoding.PEM))
        private=self.data/'server.key'
        private.write_bytes(key.private_bytes(serialization.Encoding.PEM,
                            serialization.PrivateFormat.PKCS8,serialization.NoEncryption()))
        private.chmod(0o600)

    def sql(self, sql, database='postgres', admin=False, label=None):
        name = label or f'command-{len(self.commands)}'
        command = [self.tools['psql'], '-X', '-v', 'ON_ERROR_STOP=1', '-A', '-t',
                   '-h', '127.0.0.1', '-p', str(self.port), '-U', ADMIN if admin else USER,
                   '-d', database]
        run = subprocess.run(command, input=sql, env=self.environment, text=True,
                             capture_output=True, timeout=120)
        (self.out/(name+'.stdout')).write_text(run.stdout)
        (self.out/(name+'.stderr')).write_text(run.stderr)
        self.commands.append(dict(label=name,database=database,admin=admin,exit=run.returncode))
        if run.returncode:
            raise RuntimeError(f'PostgreSQL fixture {name} failed; inspect {self.out/(name+".stderr")}')
        return run.stdout

    def reset(self):
        # This connection always names this cluster's owned loopback port explicitly.
        self.sql(f'DROP DATABASE IF EXISTS {DATABASE} WITH (FORCE);', admin=True)
        self.sql(f'CREATE DATABASE {DATABASE} TEMPLATE {TEMPLATE} OWNER {quote_identifier(USER)};', admin=True)

    def __exit__(self, *_):
        if self.started or (getattr(self,'data',self.out/'data')/'postmaster.pid').exists():
            subprocess.run([self.tools['pg_ctl'], '-D', str(self.data), '-m', 'fast', '-w', 'stop'],
                           check=True, capture_output=True, text=True, timeout=60)
            self.started = False
        (self.out/'cluster.json').write_text(json.dumps(dict(port=getattr(self,'port',None),
            server_stopped=not (self.out/'data/postmaster.pid').exists(), commands=self.commands,
            postgres=subprocess.check_output([self.tools['pg_ctl'],'--version'],text=True).strip()),indent=2)+'\n')
