"""Pinned, owned MinIO process. No Docker, user config, DNS or global routes."""
import json
import os
from pathlib import Path
import socket
import subprocess
import time
import urllib.request

ROOT_USER='duckdb_minio_admin'
ROOT_PASSWORD='duckdb_minio_admin_password'
USERS=['minio_duckdb_user','minio_duckdb_user_2']

class Minio:
    def __init__(self,tools,out):
        self.tools=Path(tools).resolve();self.out=Path(out).resolve();self.process=None;self.log=None;self.commands=[]
    def __enter__(self):
        self.out.mkdir(parents=True,exist_ok=False)
        for name in ['data','certs','mc','tmp']:(self.out/name).mkdir()
        with socket.socket() as sock:sock.bind(('127.0.0.1',0));self.port=sock.getsockname()[1]
        self.endpoint=f'http://127.0.0.1:{self.port}'
        self.env={k:v for k,v in os.environ.items() if not k.startswith(('MINIO_','MC_','AWS_','DYLD_')) and k.lower() not in ['http_proxy','https_proxy','all_proxy','no_proxy']}
        self.env.update(MINIO_ROOT_USER=ROOT_USER,MINIO_ROOT_PASSWORD=ROOT_PASSWORD,MINIO_REGION_NAME='eu-west-1',MINIO_DOMAIN='duckdb-minio.com',MINIO_BROWSER='off',MINIO_UPDATE='off',GOMAXPROCS='2',TMPDIR=str(self.out/'tmp'))
        self.log=(self.out/'server.log').open('w')
        try:
            self.process=subprocess.Popen([str(self.tools/'minio'),'server','--address',f'127.0.0.1:{self.port}','--certs-dir',str(self.out/'certs'),'--json',str(self.out/'data')],env=self.env,stdout=self.log,stderr=subprocess.STDOUT)
            deadline=time.monotonic()+20
            opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
            while True:
                if self.process.poll() is not None:raise RuntimeError('Owned MinIO exited; inspect server.log')
                try:
                    with opener.open(self.endpoint+'/minio/health/ready',timeout=.5) as response:
                        if response.status==200:break
                except Exception:pass
                if time.monotonic()>deadline:raise RuntimeError('Owned MinIO readiness timeout')
                time.sleep(.1)
            self.mc('alias','set','owned',self.endpoint,ROOT_USER,ROOT_PASSWORD)
            for name in USERS:
                self.mc('admin','user','add','owned',name,name+'_password')
                self.mc('admin','policy','attach','owned','readwrite','--user',name)
            self.reset()
            return self
        except BaseException:self.__exit__(None,None,None);raise
    def mc(self,*args):
        command=[str(self.tools/'mc'),'--config-dir',str(self.out/'mc'),'--json','--no-color','--disable-pager',*map(str,args)]
        run=subprocess.run(command,env=self.env,text=True,capture_output=True,timeout=60)
        self.commands.append(dict(args=list(map(str,args)),exit=run.returncode,stdout=run.stdout,stderr=run.stderr))
        (self.out/'commands.json').write_text(json.dumps(self.commands,indent=2)+'\n')
        if run.returncode:raise RuntimeError('Owned mc failed: '+' '.join(map(str,args))+'\n'+run.stderr+run.stdout)
        return run.stdout
    def reset(self):
        for name in ['test-bucket','test-bucket-2','test-bucket-public']:
            # A missing bucket is allowed only for this owned reset operation.
            run=subprocess.run([str(self.tools/'mc'),'--config-dir',str(self.out/'mc'),'--json','rb','--force','owned/'+name],env=self.env,text=True,capture_output=True,timeout=60)
            if run.returncode and 'does not exist' not in run.stdout+run.stderr and 'not exist' not in run.stdout+run.stderr:raise RuntimeError(run.stdout+run.stderr)
            self.mc('mb','owned/'+name)
        self.mc('version','enable','owned/test-bucket')
        self.mc('anonymous','set','public','owned/test-bucket-public')
    def __exit__(self,*_):
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:self.process.kill();self.process.wait()
        if self.log:self.log.close()
        (self.out/'server-state.json').write_text(json.dumps(dict(pid=self.process.pid if self.process else None,stopped=self.process is None or self.process.poll() is not None,endpoint=getattr(self,'endpoint',None)),indent=2)+'\n')
