"""Owned standard Python HTTP server, intentionally without Range support."""
from functools import partial
import http.server
import json
from pathlib import Path
import threading


class HttpServer:
    def __init__(self, out):
        self.out = Path(out).resolve()

    def __enter__(self):
        self.out.mkdir(parents=True, exist_ok=False)
        self.directory = self.out / 'files'
        self.directory.mkdir()
        self.log = (self.out / 'requests.jsonl').open('w')
        lock = threading.Lock()
        log = self.log

        class Handler(http.server.SimpleHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def log_request(self, code='-', size='-'):
                with lock:
                    log.write(json.dumps(dict(method=self.command, path=self.path, status=code,
                                              range=self.headers.get('Range'),
                                              user_agent=self.headers.get('User-Agent'))) + '\n')
                    log.flush()

            def copyfile(self, source, outputfile):
                try:
                    super().copyfile(source, outputfile)
                except (BrokenPipeError, ConnectionResetError):
                    self.close_connection = True

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0),
            partial(Handler, directory=str(self.directory)))
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.log.close()
        (self.out / 'server-state.json').write_text(json.dumps(dict(url=self.url, stopped=True)) + '\n')
