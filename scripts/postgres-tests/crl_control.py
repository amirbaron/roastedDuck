#!/usr/bin/env python3
"""Compare file-based CRL and TLS policy behavior on owned certificates/servers."""
import argparse
from datetime import datetime,timedelta,timezone
import hashlib
import json
from pathlib import Path
import subprocess
from cryptography import x509
from cryptography.hazmat.primitives import hashes,serialization
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.x509.oid import NameOID
from cluster import Cluster
from codec_control import literal

def option(value):return "'"+str(value).replace('\\','\\\\').replace("'","\\'")+"'"

class RevocationCluster(Cluster):
    def _certificate(self):
        super()._certificate()
        now=datetime.now(timezone.utc)
        for label in ['clean','profile-clean','profile-revoked','revoked','expired','future','wrong-issuer','bad-signature','unknown-critical']:
            issuer=self.root_certificate.subject
            key=self.root_key
            if label in ['wrong-issuer','bad-signature','unknown-critical']:
                key=rsa.generate_private_key(public_exponent=65537,key_size=2048)
            if label=='wrong-issuer':issuer=x509.Name([x509.NameAttribute(NameOID.COMMON_NAME,'Unrelated fixture CA')])
            start=now+timedelta(days=1) if label=='future' else now-timedelta(days=2)
            end=now-timedelta(days=1) if label=='expired' else now+timedelta(days=2)
            builder=x509.CertificateRevocationListBuilder().issuer_name(issuer).last_update(start).next_update(end)
            if label in ['revoked','profile-revoked']:
                builder=builder.add_revoked_certificate(x509.RevokedCertificateBuilder().serial_number(self.server_certificate.serial_number).revocation_date(now-timedelta(hours=1)).build())
            if label.startswith('profile-'):builder=builder.add_extension(x509.CRLNumber(1),critical=False).add_extension(x509.AuthorityKeyIdentifier.from_issuer_public_key(key.public_key()),critical=False)
            if label=='unknown-critical':builder=builder.add_extension(x509.UnrecognizedExtension(x509.ObjectIdentifier('1.2.3.4'),b'\x05\x00'),critical=True)
            crl=builder.sign(key,hashes.SHA256())
            (self.out/(label+'.crl')).write_bytes(crl.public_bytes(serialization.Encoding.PEM))
        (self.out/'invalid.crl').write_text('not a certificate revocation list\n')
        (self.out/'certificate.crl').write_bytes(self.root_certificate.public_bytes(serialization.Encoding.PEM))

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for name in ['cli','oracle','extension','out']:parser.add_argument('--'+name,type=Path,required=True)
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False);results=[]
    with RevocationCluster(out/'cluster') as cluster:
        cluster.reset()
        cases=[(label,'verify-full',label not in ['revoked','profile-revoked','expired','future','wrong-issuer','bad-signature','unknown-critical','certificate'],False)
               for label in ['clean','profile-clean','profile-revoked','missing','invalid','revoked','expired','future','wrong-issuer','bad-signature','unknown-critical','certificate']]
        cases += [('revoked','require',False,False),('clean','verify-ca',True,True),('revoked','verify-ca',False,True),('clean','verify-full',False,True)]
        for label,mode,expected,mismatch in cases:
            runs={};name=label+'-'+mode+('-wrong-host' if mismatch else '')
            for engine in ['cpp','rust']:
                directory=out/(name+'-'+engine);directory.mkdir();(directory/'secrets').mkdir()
                dsn='dbname=postgresscanner sslmode='+mode+' sslcrl='+option(cluster.out/(label+'.crl'))
                if mismatch:dsn+=' host=wrong.fixture hostaddr=127.0.0.1'
                load='LOAD '+literal(str(args.extension.resolve()))+';' if engine=='cpp' else 'LOAD postgres;'
                sql='SET secret_directory='+literal(str(directory/'secrets'))+';'+load+'ATTACH '+literal(dsn)+" AS p(TYPE postgres); SELECT * FROM postgres_query('p','SELECT 42::integer n'); DETACH p;"
                binary=args.oracle if engine=='cpp' else args.cli
                run=subprocess.run([str(binary.resolve()),'-batch','-bail','-csv','-noheader','-init','/dev/null',':memory:'],input=sql,text=True,capture_output=True,env=cluster.environment,timeout=30)
                (directory/'query.sql').write_text(sql);(directory/'stdout').write_text(run.stdout);(directory/'stderr').write_text(run.stderr);runs[engine]=run
            passed=all(r.returncode==0 and r.stdout.strip()=='42' for r in runs.values()) if expected else all(r.returncode!=0 and not r.stdout.strip() and ('SSL' in r.stderr or 'TLS' in r.stderr or 'tls' in r.stderr or 'certificate' in r.stderr or 'CRL' in r.stderr) for r in runs.values())
            record=dict(name=name,expected_success=expected,cpp_exit=runs['cpp'].returncode,rust_exit=runs['rust'].returncode,passed=passed);results.append(record);print(json.dumps(record),flush=True)
    report=dict(cases=results,server_stopped=json.loads((out/'cluster/cluster.json').read_text())['server_stopped'],sha256={n:hashlib.sha256(getattr(args,n).read_bytes()).hexdigest() for n in ['cli','oracle','extension']})
    (out/'verification.json').write_text(json.dumps(report,indent=2)+'\n')
    if not report['server_stopped'] or not all(c['passed'] for c in results):raise SystemExit(1)
if __name__=='__main__':main()
