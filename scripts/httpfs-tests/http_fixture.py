"""Owned loopback HTTP fixtures: a plain file server without Range support, and a
forward proxy for plain-HTTP origins."""
import base64
from functools import partial
import http.client
import http.server
import json
from pathlib import Path
import threading
import urllib.parse


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

            def handle(self):
                try:
                    super().handle()
                except (BrokenPipeError, ConnectionResetError):
                    self.close_connection = True

            def log_request(self, code='-', size='-'):
                with lock:
                    headers = getattr(self, 'headers', {})
                    log.write(json.dumps(dict(method=getattr(self, 'command', None), path=getattr(self, 'path', None), status=code,
                                              range=headers.get('Range'),
                                              user_agent=headers.get('User-Agent'))) + '\n')
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


class ForwardProxy:
    """A forward proxy for absolute-form requests to plain-HTTP origins.

    `origins` maps a requested host suffix to the loopback port serving it; any other
    origin is refused. With `credentials` (user, password) every request needs that
    Basic proxy authorization, else it gets 407. Requests are logged without their
    authorization values.
    """

    def __init__(self, out, origins, credentials=None):
        self.out = Path(out).resolve()
        self.origins = dict(origins)
        self.credentials = credentials

    def __enter__(self):
        self.out.mkdir(parents=True, exist_ok=False)
        self.log = (self.out / 'requests.jsonl').open('w')
        lock, log, origins = threading.Lock(), self.log, self.origins
        expected = None
        if self.credentials:
            expected = 'Basic ' + base64.b64encode(':'.join(self.credentials).encode()).decode()

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = 'HTTP/1.1'

            def log_message(self, *args):
                pass

            def handle(self):
                try:
                    super().handle()
                except (BrokenPipeError, ConnectionResetError):
                    self.close_connection = True

            def reply(self, status, headers, body):
                with lock:
                    log.write(json.dumps(dict(method=self.command, target=self.path, status=status,
                                              authorized=self.headers.get('Proxy-Authorization') is not None)) + '\n')
                    log.flush()
                self.send_response(status)
                for name, value in headers:
                    self.send_header(name, value)
                self.send_header('Content-Length', str(len(body)))
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(body)

            def forward(self):
                payload = self.rfile.read(int(self.headers.get('Content-Length') or 0))
                if expected and self.headers.get('Proxy-Authorization') != expected:
                    return self.reply(407, [('Proxy-Authenticate', 'Basic realm="owned fixture"')], b'')
                target = urllib.parse.urlsplit(self.path)
                port = next((port for suffix, port in origins.items()
                             if target.scheme == 'http' and target.hostname
                             and (target.hostname == suffix or target.hostname.endswith('.' + suffix))), None)
                if port is None:
                    return self.reply(403, [], b'origin is not an owned fixture')
                headers = {name: value for name, value in self.headers.items()
                           if name.lower() not in ['proxy-authorization', 'proxy-connection', 'connection', 'keep-alive']}
                connection = http.client.HTTPConnection('127.0.0.1', port, timeout=60)
                try:
                    path = target.path + ('?' + target.query if target.query else '')
                    connection.request(self.command, path, body=payload or None, headers=headers)
                    response = connection.getresponse()
                    body = response.read()
                    kept = [(name, value) for name, value in response.getheaders()
                            if name.lower() not in ['connection', 'keep-alive', 'transfer-encoding', 'content-length']]
                    if self.command == 'HEAD' and response.getheader('Content-Length') is not None:
                        # A HEAD reply states the entity length without carrying it.
                        self.send_response(response.status)
                        for name, value in kept:
                            self.send_header(name, value)
                        self.send_header('Content-Length', response.getheader('Content-Length'))
                        self.end_headers()
                        with lock:
                            log.write(json.dumps(dict(method=self.command, target=self.path, status=response.status,
                                                      authorized=self.headers.get('Proxy-Authorization') is not None)) + '\n')
                            log.flush()
                        return
                    self.reply(response.status, kept, body)
                finally:
                    connection.close()

            do_GET = do_HEAD = do_PUT = do_POST = do_DELETE = forward

            def do_CONNECT(self):
                self.reply(405, [], b'')

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.address = f'127.0.0.1:{self.server.server_port}'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.log.close()
        (self.out / 'server-state.json').write_text(json.dumps(dict(address=self.address, stopped=True)) + '\n')
