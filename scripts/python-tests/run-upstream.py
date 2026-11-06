"""Run unmodified pinned upstream Python tests with bounded, isolated SSD output."""
from pathlib import Path
import argparse,json,os,subprocess,sys,xml.etree.ElementTree as ET
parser=argparse.ArgumentParser();parser.add_argument('--python',required=True);parser.add_argument('--source',required=True);parser.add_argument('--output',required=True);parser.add_argument('--rust',action='store_true');parser.add_argument('tests',nargs='+');args=parser.parse_args()
output=Path(args.output).resolve();output.mkdir(parents=True,exist_ok=True)
if (output/'test.log').exists():raise SystemExit('Use a fresh output directory to preserve prior evidence')
source=Path(args.source).resolve();work=output/'work';work.mkdir();env=os.environ.copy();env['TMPDIR']=str(output/'tmp');Path(env['TMPDIR']).mkdir();env['PYTHONPYCACHEPREFIX']=str(output/'pycache')
interpreter=str(Path(args.python).resolve())
# Keep the virtualenv entry point (resolving its symlink would select the base interpreter).
interpreter=str(Path(args.python).absolute())
if args.rust:
 with (output/'identity.json').open('w')as log:subprocess.run([interpreter,str(Path(__file__).with_name('identity.py').resolve())],cwd=work,env=env,stdout=log,stderr=subprocess.STDOUT,check=True)
command=['nice','-n','10',interpreter,'-m','pytest','-q','--continue-on-collection-errors','-o','cache_dir='+str(output/'cache'),'--basetemp='+str(output/'pytest-tmp'),'--junitxml='+str(output/'junit.xml'),*[str(source/test)for test in args.tests]]
with (output/'test.log').open('w')as log:run=subprocess.run(command,cwd=work,env=env,stdout=log,stderr=subprocess.STDOUT)
report={'exit_code':run.returncode,'tests':args.tests,'python':interpreter,'source':str(source),'rust_identity_checked':args.rust}
if (output/'junit.xml').exists():
 suites=ET.parse(output/'junit.xml').getroot();report['suites']=[dict(suite.attrib)for suite in suites.iter('testsuite')]
(output/'summary.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report));raise SystemExit(run.returncode)
