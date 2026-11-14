"""Local S3 reverse proxy with independent SigV4 and fixture policy validation."""
import hashlib
import base64
import hmac
import http.client
import http.server
import threading
import urllib.parse


class SignedS3Fixture:
    def __init__(self, backend, credentials, region):
        self.backend = urllib.parse.urlsplit(backend)
        self.credentials = credentials
        self.region = region
        self.requests = []

    def __enter__(self):
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def body(self):
                if self.headers.get('Transfer-Encoding', '').lower() == 'chunked':
                    chunks = []
                    while True:
                        size = int(self.rfile.readline().split(b';', 1)[0], 16)
                        if not size:
                            while self.rfile.readline().strip():
                                pass
                            return b''.join(chunks)
                        chunks.append(self.rfile.read(size))
                        assert self.rfile.read(2) == b'\r\n'
                return self.rfile.read(int(self.headers.get('Content-Length', '0')))

            def authenticate(self, body):
                try:
                    algorithm, auth = self.headers['Authorization'].split(' ', 1)
                    auth = dict(p.strip().split('=', 1) for p in auth.split(','))
                    key, date, region, service, end = auth['Credential'].split('/')
                    if region != owner.region:
                        return 400, 'AuthorizationHeaderMalformed', None
                    secret, token = owner.credentials[key]
                    assert algorithm == 'AWS4-HMAC-SHA256' and service == 's3' and end == 'aws4_request'
                    assert self.headers.get('x-amz-security-token', '') == token
                    assert self.headers['x-amz-date'].startswith(date)
                    parsed = urllib.parse.urlsplit(self.path)
                    encode = lambda v: urllib.parse.quote(v, safe='-_.~')
                    uri = urllib.parse.quote(urllib.parse.unquote(parsed.path), safe='/-_.~')
                    query = '&'.join(k + '=' + v for k, v in sorted(
                        (encode(k), encode(v)) for k, v in urllib.parse.parse_qsl(parsed.query, keep_blank_values=True)))
                    signed = auth['SignedHeaders']
                    headers = ''.join(k + ':' + ' '.join(self.headers[k].split()) + '\n' for k in signed.split(';'))
                    payload = hashlib.sha256(body).hexdigest()
                    assert self.headers.get('x-amz-content-sha256', payload) == payload
                    canonical = '\n'.join([self.command, uri, query, headers, signed, payload])
                    scope = '/'.join([date, region, service, end])
                    to_sign = '\n'.join([algorithm, self.headers['x-amz-date'], scope, hashlib.sha256(canonical.encode()).hexdigest()])
                    signing_key = ('AWS4' + secret).encode()
                    for part in [date, region, service, end]:
                        signing_key = hmac.new(signing_key, part.encode(), hashlib.sha256).digest()
                    assert hmac.compare_digest(hmac.new(signing_key, to_sign.encode(), hashlib.sha256).hexdigest(), auth['Signature'])
                    return 200, '', True
                except (KeyError, ValueError, AssertionError, AttributeError, TypeError):
                    return 403, 'SignatureDoesNotMatch', False

            def handle_request(self):
                body = self.body()
                status, code, signature = self.authenticate(body)
                owner.requests.append({'method': self.command, 'path': self.path, 'auth_status': status,
                                       'valid_signature': signature})
                if status != 200:
                    payload = f'<Error><Code>{code}</Code><Message>local S3 fixture policy</Message></Error>'.encode()
                    self.send_response(status)
                    self.send_header('Content-Length', str(len(payload)))
                    self.send_header('x-amz-error-code', code)
                    self.end_headers()
                    if self.command != 'HEAD':
                        self.wfile.write(payload)
                    return
                backend = http.client.HTTPConnection(owner.backend.hostname, owner.backend.port, timeout=10)
                try:
                    headers = {k: v for k, v in self.headers.items() if k.lower() not in ['connection', 'transfer-encoding']}
                    headers['Content-Length'] = str(len(body))
                    backend.request(self.command, self.path, body, headers)
                    response = backend.getresponse()
                    payload = response.read()
                    self.send_response(response.status)
                    for name, value in response.getheaders():
                        if name.lower() not in ['connection', 'transfer-encoding', 'server', 'date', 'content-length']:
                            self.send_header(name, value)
                    self.send_header('Content-Length', response.getheader('Content-Length', str(len(payload))))
                    self.end_headers()
                    if self.command != 'HEAD':
                        self.wfile.write(payload)
                finally:
                    backend.close()

            do_GET = do_HEAD = do_PUT = do_POST = do_DELETE = handle_request

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.endpoint = f'127.0.0.1:{self.server.server_port}'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class ForwardProxy:
    """Loopback-only HTTP proxy; records traffic and enforces optional Basic auth."""
    def __init__(self, endpoint, log_path, authenticated=False):
        self.endpoint = endpoint
        self.log_path = log_path
        self.authenticated = authenticated
        self.requests = []

    def __enter__(self):
        owner = self
        owner.log_path.write_text('')

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def handle_request(self):
                parsed = urllib.parse.urlsplit(self.path)
                expected = 'Basic ' + base64.b64encode(b'admin:password').decode()
                authorized = not owner.authenticated or self.headers.get('Proxy-Authorization') == expected
                owner.requests.append(dict(method=self.command, path=self.path, authorized=authorized))
                if not authorized or parsed.netloc != owner.endpoint or parsed.scheme != 'http':
                    self.send_response(407 if not authorized else 403)
                    self.send_header('Proxy-Authenticate', 'Basic realm="local fixture"')
                    self.send_header('Content-Length', '0')
                    self.end_headers()
                    return
                with owner.log_path.open('a') as log:
                    log.write(self.command + ' ' + self.path + '\n')
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                target = http.client.HTTPConnection(owner.endpoint, timeout=10)
                try:
                    headers = {k: v for k, v in self.headers.items()
                               if k.lower() not in ['proxy-authorization', 'proxy-connection', 'connection']}
                    target.request(self.command, urllib.parse.urlunsplit(('', '', parsed.path, parsed.query, '')), body, headers)
                    response = target.getresponse()
                    payload = response.read()
                    self.send_response(response.status)
                    for name, value in response.getheaders():
                        if name.lower() not in ['connection', 'transfer-encoding', 'server', 'date']:
                            self.send_header(name, value)
                    self.end_headers()
                    if self.command != 'HEAD':
                        self.wfile.write(payload)
                finally:
                    target.close()

            do_GET = do_HEAD = do_PUT = do_POST = do_DELETE = handle_request

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
