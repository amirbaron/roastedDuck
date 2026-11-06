"""Sign native extensions in a built wheel and regenerate RECORD, preserving the original wheel."""
from pathlib import Path
import base64,csv,hashlib,io,subprocess,sys,zipfile
wheel=Path(sys.argv[1]);destination=Path(sys.argv[2]);workspace=Path(sys.argv[3]);destination.mkdir(parents=True,exist_ok=True)
with zipfile.ZipFile(wheel) as archive:
 data={entry.filename:archive.read(entry.filename) for entry in archive.infolist() if not entry.is_dir()}
for name in data:
 if name.endswith(('.so','.dylib')):
  path=destination/'native'/name;path.parent.mkdir(parents=True,exist_ok=True);path.write_bytes(data[name]);subprocess.run([str(workspace/'scripts/sign.sh'),str(path)],check=True);data[name]=path.read_bytes()
record=next(name for name in data if name.endswith('.dist-info/RECORD'));output=io.StringIO();writer=csv.writer(output,lineterminator='\n')
for name,content in sorted(data.items()):
 if name==record:continue
 digest=base64.urlsafe_b64encode(hashlib.sha256(content).digest()).rstrip(b'=').decode();writer.writerow([name,'sha256='+digest,str(len(content))])
writer.writerow([record,'','']);data[record]=output.getvalue().encode();result=destination/wheel.name
with zipfile.ZipFile(result,'w',compression=zipfile.ZIP_DEFLATED) as archive:
 for name,content in data.items():archive.writestr(name,content)
print(result)
