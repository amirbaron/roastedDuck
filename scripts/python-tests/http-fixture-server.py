"""Loopback fixture process; imports no DuckDB engine and has a bounded lifetime."""
import base64
import http.server
import json
import os
from pathlib import Path
import re
import sys
import threading
import time
import urllib.parse

config = json.loads(Path(sys.argv[1]).read_text())
output = Path(config["output"])
payloads = {name: base64.b64decode(data) for name, data in config["payloads"].items()}
lock = threading.Lock()
request_count = 0


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def date_time_string(self, timestamp=None):
        return "Wed, 01 Jan 2020 00:00:00 GMT"

    def do_HEAD(self):
        self.respond(False)

    def do_GET(self):
        self.respond(True)

    def respond(self, body):
        global request_count
        path = urllib.parse.urlsplit(self.path).path
        with lock:
            request_count += 1
            assert request_count <= 200
            record = {
                "method": self.command,
                "path": self.path,
                "range": self.headers.get("Range"),
                "s3_authentication_present": self.headers.get("Authorization", "").startswith(
                    "AWS4-HMAC-SHA256 Credential=fixture-key/"
                ),
            }
            with (output / "server-requests.jsonl").open("a") as stream:
                stream.write(json.dumps(record) + "\n")
        try:
            if path == "/redirect.csv":
                self.send_response(302)
                self.send_header("Location", "/data.csv")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
            payload = payloads.get(path)
            status = 200 if payload is not None else 404
            if payload is None:
                payload = b"The extension fixture is missing\n"
            start, end = 0, len(payload) - 1
            if status == 200 and body and self.headers.get("Range"):
                match = re.fullmatch(r"bytes=(\d+)-(\d+)", self.headers["Range"])
                assert match
                start = int(match[1])
                end = min(int(match[2]), len(payload) - 1)
                status = 206
            self.send_response(status)
            self.send_header("Content-Length", str(end - start + 1 if status == 206 else len(payload)))
            self.send_header("ETag", '"duckdb-python-extension-fixture-v1"')
            self.send_header("X-Parity-Fixture", "1522")
            if status == 206:
                self.send_header("Content-Range", f"bytes {start}-{end}/{len(payload)}")
            self.end_headers()
            if body:
                self.wfile.write(payload[start:end + 1] if status == 206 else payload)
        except (BrokenPipeError, ConnectionResetError):
            pass


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
# Publish readiness only after the complete address is visible to readers.
address_tmp = output / ".server-address.json.tmp"
address_tmp.write_text(
    json.dumps({"pid": os.getpid(), "port": server.server_port}) + "\n"
)
os.replace(address_tmp, output / "server-address.json")


def lifetime_guard():
    started = time.monotonic()
    while time.monotonic() - started < 120:
        try:
            os.kill(config["owner_pid"], 0)
        except ProcessLookupError:
            break
        time.sleep(1)
    server.shutdown()


threading.Thread(target=lifetime_guard, daemon=True).start()
try:
    server.serve_forever()
finally:
    server.server_close()
