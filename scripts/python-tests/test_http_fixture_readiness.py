"""A reader must never observe an incomplete fixture readiness document."""
import base64
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request


class ReadinessPublicationTest(unittest.TestCase):
    def test_first_visible_address_is_complete(self):
        server = Path(os.environ.get("RUSTDB_HTTP_FIXTURE_SERVER", str(Path(__file__).with_name("http-fixture-server.py")))).resolve()
        with tempfile.TemporaryDirectory(prefix="http-readiness-") as temporary:
            root = Path(temporary)
            config = root / "input.json"
            payload = b"i,s\n1,one\n"
            config.write_text(json.dumps({"owner_pid": os.getpid(), "output": str(root), "payloads": {"/data.csv": base64.b64encode(payload).decode()}}))
            wrapper = root / "slow-storage.py"
            wrapper.write_text("""import pathlib,runpy,sys,time
original = pathlib.Path.write_text
def delayed_write(self, data, *args, **kwargs):
    if 'server-address' in self.name:
        with self.open('w', encoding='utf-8') as stream:
            stream.flush()
            time.sleep(0.1)
            return stream.write(data)
    return original(self, data, *args, **kwargs)
pathlib.Path.write_text = delayed_write
script = sys.argv[1]
sys.argv = sys.argv[1:]
runpy.run_path(script, run_name='__main__')
""")
            with (root / "server.log").open("w") as log:
                process = subprocess.Popen([sys.executable, "-B", str(wrapper), str(server), str(config)], stdout=log, stderr=subprocess.STDOUT)
                try:
                    deadline = time.monotonic() + 10
                    address_path = root / "server-address.json"
                    while not address_path.is_file():
                        self.assertIsNone(process.poll(), "server exited before readiness")
                        self.assertLess(time.monotonic(), deadline, "readiness timeout")
                        time.sleep(0.001)
                    address = json.loads(address_path.read_text())
                    self.assertEqual(address["pid"], process.pid)
                    with urllib.request.urlopen("http://127.0.0.1:" + str(address["port"]) + "/data.csv", timeout=5) as response:
                        self.assertEqual(response.read(), payload)
                        self.assertEqual(response.headers["X-Parity-Fixture"], "1522")
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    unittest.main()
