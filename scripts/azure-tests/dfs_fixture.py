"""Disposable ADLS REST fixture with independent SharedKey checks and staged writes."""
import base64
import hashlib
import hmac
import http.server
import json
import ssl
import threading
import traceback
import urllib.parse
import uuid

ACCOUNT = 'roastedduckfixture'
KEY = base64.b64encode(b'local-dfs-fixture-secret-key-32!!').decode()
DATE = 'Sun, 15 Nov 2026 00:00:00 GMT'


class DfsFixture:
    def __init__(self, tls_context=None, bearer_token=None):
        self.entries = {'': None, 'folder': None, 'folder/a': None, 'folder/b': None,
                        'input.csv': b'value\n1\n2\n3\n',
                        'folder/a/one.csv': b'value\n1\n', 'folder/b/two.csv': b'value\n2\n'}
        self.pending = {}
        self.requests = []
        self.lock = threading.RLock()
        self.tls_context = tls_context
        self.bearer_token = bearer_token
        self.disconnects = 0
        self.signing_account = ACCOUNT
        self.list_page_size = 2
        self.etag = '"fixture-etag"'
        self.server_errors = []
        self.server_error_count = 0

    def before_request(self, handler, parsed, body):
        pass

    def after_response(self, handler, status):
        pass

    def current_etag(self,handler,parsed):
        return self.etag

    def response_headers(self,handler,status):
        return {}

    def path_properties(self,handler,name):
        return dict(lastModified=DATE,etag=self.etag.strip('"'))

    def locate(self, handler, parsed):
        prefix = f'/{ACCOUNT}/fixture'
        if parsed.path != prefix and not parsed.path.startswith(prefix + '/'):
            return None
        path = urllib.parse.unquote(parsed.path[len(prefix):].lstrip('/')).rstrip('/')
        return self.entries, self.pending, path

    def allow_anonymous(self, handler, parsed):
        return False

    def handle_storage(self, handler, parsed, body, entries, pending, path):
        return False

    def error_reason(self, code):
        return None

    def handle_identity(self, handler, parsed, body):
        return False

    def __enter__(self):
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            # C++ Azure's custom curl transport waits for an explicit 100 response.
            protocol_version = 'HTTP/1.1'
            def handle(self):
                try:
                    super().handle()
                except (ConnectionResetError, BrokenPipeError, ssl.SSLEOFError):
                    owner.disconnects += 1

            def log_message(self, *_):
                pass

            def read_body(self):
                if self.headers.get('Transfer-Encoding','').lower() == 'chunked':
                    chunks=[]
                    while True:
                        size=int(self.rfile.readline().split(b';',1)[0],16)
                        if size==0:
                            while self.rfile.readline().strip():
                                pass
                            return b''.join(chunks)
                        chunks.append(self.rfile.read(size))
                        assert self.rfile.read(2)==b'\r\n'
                return self.rfile.read(int(self.headers.get('Content-Length','0')))

            def authenticate(self, parsed):
                if not self.headers.get('Authorization') and owner.allow_anonymous(self, parsed):
                    return True
                if owner.bearer_token is not None and self.headers.get('Authorization', '').startswith('Bearer '):
                    return hmac.compare_digest(self.headers['Authorization'], 'Bearer ' + owner.bearer_token)
                fields = [self.command]
                for name in ['content-encoding', 'content-language', 'content-length', 'content-md5',
                             'content-type', 'date', 'if-modified-since', 'if-match', 'if-none-match',
                             'if-unmodified-since', 'range']:
                    value = self.headers.get(name, '')
                    fields.append('' if name == 'content-length' and value == '0' else value)
                canonical = '\n'.join(fields) + '\n'
                canonical += ''.join(f'{key.lower()}:{value.strip()}\n' for key, value in
                                     sorted(self.headers.items(), key=lambda h: h[0].lower())
                                     if key.lower().startswith('x-ms-'))
                canonical += '/' + owner.signing_account + parsed.path
                for name, values in sorted(urllib.parse.parse_qs(parsed.query, keep_blank_values=True).items()):
                    canonical += '\n' + name.lower() + ':' + ','.join(sorted(values))
                expected = base64.b64encode(hmac.new(base64.b64decode(KEY), canonical.encode(), hashlib.sha256).digest()).decode()
                return hmac.compare_digest(self.headers.get('Authorization', ''), f'SharedKey {owner.signing_account}:{expected}')

            def respond(self, status, body=b'', headers=None, length=None, reason=None):
                self.close_connection = True
                self.send_response(status, reason)
                common = {'Content-Length': str(len(body) if length is None else length), 'Connection':'close',
                          'Last-Modified': DATE, 'ETag': owner.current_etag(self,urllib.parse.urlsplit(self.path)),
                          'x-ms-version': '2025-07-05', 'x-ms-request-id': str(uuid.uuid4()),
                          'x-ms-creation-time': DATE, 'x-ms-blob-type': 'BlockBlob',
                          'x-ms-request-server-encrypted': 'true', 'x-ms-server-encrypted': 'true'}
                common.update(headers or {})
                common.update(owner.response_headers(self,status))
                for name, value in common.items():
                    self.send_header(name, value)
                self.end_headers()
                try:
                    if self.command != 'HEAD':
                        self.wfile.write(body)
                finally:
                    owner.after_response(self, status)

            def error(self, status, code):
                if self.command == 'HEAD':
                    return self.respond(status, headers={'x-ms-error-code': code}, reason=owner.error_reason(code))
                self.respond(status, json.dumps({'error': {'code': code, 'message': code}}).encode(),
                             {'x-ms-error-code': code, 'Content-Type': 'application/json'}, reason=owner.error_reason(code))

            def handle_request(self):
                parsed = urllib.parse.urlsplit(self.path)
                body = self.read_body()
                if owner.handle_identity(self, parsed, body):
                    return
                owner.before_request(self, parsed, body)
                valid = self.authenticate(parsed)
                shared_key = self.headers.get('Authorization', '').startswith('SharedKey ')
                request = dict(method=self.command, host=self.headers.get('Host'), path=self.path, valid_signature=valid if shared_key else None,
                               valid_authorization=valid, authentication='shared_key' if shared_key else
                               ('bearer' if self.headers.get('Authorization') else 'anonymous'), body_length=len(body),
                               content_length=self.headers.get('Content-Length'), transfer_encoding=self.headers.get('Transfer-Encoding'),
                               expect=self.headers.get('Expect'), range=self.headers.get('Range',self.headers.get('x-ms-range')),
                               if_match=self.headers.get('If-Match'))
                with owner.lock:
                    owner.requests.append(request)
                    if not valid:
                        return self.error(403, 'AuthenticationFailed')
                    if self.headers.get('If-Match') not in [None, '*', owner.current_etag(self,parsed)]:
                        return self.error(412, 'ConditionNotMet')
                    location = owner.locate(self, parsed)
                    if location is None:
                        return self.error(404, 'ContainerNotFound')
                    entries, pending, path = location
                    if owner.handle_storage(self, parsed, body, entries, pending, path):
                        return
                    query = {k: v[-1] for k, v in urllib.parse.parse_qs(parsed.query, keep_blank_values=True).items()}
                    if self.command == 'GET' and query.get('resource') == 'filesystem':
                        directory = query.get('directory', '')
                        if directory not in entries or entries[directory] is not None:
                            return self.error(404, 'PathNotFound')
                        prefix = directory + '/' if directory else ''
                        names = [name for name in sorted(entries) if name and name.startswith(prefix)
                                 and (query.get('recursive') == 'true' or '/' not in name[len(prefix):])]
                        offset = int(query.get('continuation') or '0')
                        page_size = min(owner.list_page_size, int(query.get('maxResults') or owner.list_page_size))
                        page = names[offset:offset + page_size]
                        payload = {'paths': [dict(name=name, isDirectory=entries[name] is None,
                                                  contentLength=str(len(entries[name] or b'')),
                                                  **owner.path_properties(self,name), owner='fixture',
                                                  group='fixture', permissions='rwxrwxrwx') for name in page]}
                        headers = {'Content-Type': 'application/json'}
                        if offset + page_size < len(names):
                            headers['x-ms-continuation'] = str(offset + page_size)
                        return self.respond(200, json.dumps(payload).encode(), headers)
                    if self.command == 'PUT' and query.get('resource') in ['file', 'directory']:
                        parent = path.rsplit('/', 1)[0] if '/' in path else ''
                        if parent not in entries or entries[parent] is not None:
                            return self.error(404, 'ParentNotFound')
                        entries[path] = None if query['resource'] == 'directory' else b''
                        pending.pop(path, None)
                        return self.respond(201)
                    if path not in entries:
                        return self.error(404, 'PathNotFound')
                    data = entries[path]
                    if self.command == 'HEAD':
                        headers = {'x-ms-resource-type': 'directory' if data is None else 'file'}
                        if data is None:
                            headers['x-ms-meta-hdi_isfolder'] = 'true'
                        return self.respond(200, headers=headers, length=len(data or b''))
                    if self.command == 'GET':
                        if data is None:
                            return self.error(409, 'PathIsDirectory')
                        if 'Range' in self.headers or 'x-ms-range' in self.headers:
                            start, end = self.headers.get('Range', self.headers.get('x-ms-range'))[6:].split('-')
                            start = int(start)
                            end = min(int(end) if end else len(data) - 1, len(data) - 1)
                            if start >= len(data):
                                return self.error(416, 'InvalidRange')
                            return self.respond(206, data[start:end + 1], {'Content-Range': f'bytes {start}-{end}/{len(data)}'})
                        return self.respond(200, data)
                    if self.command == 'PATCH' and query.get('action') == 'append':
                        staged = pending.setdefault(path, bytearray(data or b''))
                        position = int(query['position'])
                        if position != len(staged):
                            return self.error(400, 'InvalidAppendOperation')
                        staged.extend(body)
                        return self.respond(202)
                    if self.command == 'PATCH' and query.get('action') == 'flush':
                        staged = pending.get(path, data or b'')
                        position = int(query['position'])
                        if position > len(staged):
                            return self.error(400, 'InvalidFlushPosition')
                        entries[path] = bytes(staged[:position])
                        if query.get('retainUncommittedData') != 'true':
                            pending.pop(path, None)
                        request['closed'] = query.get('close') == 'true'
                        return self.respond(200)
                    if self.command == 'DELETE':
                        children = [name for name in entries if name.startswith(path + '/')]
                        if children and query.get('recursive') != 'true':
                            return self.error(409, 'DirectoryNotEmpty')
                        for name in children + [path]:
                            entries.pop(name, None)
                            pending.pop(name, None)
                        return self.respond(200)
                    return self.error(400, 'UnsupportedFixtureOperation')

            do_GET = do_HEAD = do_POST = do_PUT = do_PATCH = do_DELETE = handle_request

        class Server(http.server.ThreadingHTTPServer):
            request_queue_size = 256

            def finish_request(self, request, address):
                if owner.tls_context:
                    # Handshakes run in workers, so one slow TLS client cannot
                    # block accept() for the upstream 100-connection cases.
                    try:
                        with owner.tls_context.wrap_socket(request, server_side=True) as secured:
                            super().finish_request(secured, address)
                    except (ssl.SSLError, ConnectionError):
                        owner.disconnects += 1
                else:
                    super().finish_request(request, address)

            def handle_error(self, request, address):
                owner.server_error_count += 1
                if len(owner.server_errors) < 20:
                    owner.server_errors.append(traceback.format_exc()[-5000:])

        self.server = Server(('127.0.0.1', 0), Handler)
        scheme = 'https' if self.tls_context else 'http'
        self.endpoint = f'{scheme}://127.0.0.1:{self.server.server_port}/{ACCOUNT}'
        self.connection = f'DefaultEndpointsProtocol={scheme};AccountName={ACCOUNT};AccountKey={KEY};BlobEndpoint={self.endpoint};DfsEndpoint={self.endpoint};'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
