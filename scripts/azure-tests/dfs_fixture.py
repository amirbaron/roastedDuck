"""Disposable ADLS REST fixture with independent SharedKey checks and staged writes."""
import base64
import hashlib
import hmac
import http.server
import json
import threading
import urllib.parse
import uuid

ACCOUNT = 'roastedduckfixture'
KEY = base64.b64encode(b'local-dfs-fixture-secret-key-32!!').decode()
DATE = 'Sun, 15 Nov 2026 00:00:00 GMT'


class DfsFixture:
    def __init__(self):
        self.entries = {'': None, 'folder': None, 'folder/a': None, 'folder/b': None,
                        'input.csv': b'value\n1\n2\n3\n',
                        'folder/a/one.csv': b'value\n1\n', 'folder/b/two.csv': b'value\n2\n'}
        self.pending = {}
        self.requests = []
        self.lock = threading.RLock()

    def __enter__(self):
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def authenticate(self, parsed):
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
                canonical += '/' + ACCOUNT + parsed.path
                for name, values in sorted(urllib.parse.parse_qs(parsed.query, keep_blank_values=True).items()):
                    canonical += '\n' + name.lower() + ':' + ','.join(sorted(values))
                expected = base64.b64encode(hmac.new(base64.b64decode(KEY), canonical.encode(), hashlib.sha256).digest()).decode()
                return hmac.compare_digest(self.headers.get('Authorization', ''), f'SharedKey {ACCOUNT}:{expected}')

            def respond(self, status, body=b'', headers=None, length=None):
                self.send_response(status)
                common = {'Content-Length': str(len(body) if length is None else length),
                          'Last-Modified': DATE, 'ETag': '"fixture-etag"',
                          'x-ms-version': '2025-07-05', 'x-ms-request-id': str(uuid.uuid4()),
                          'x-ms-creation-time': DATE, 'x-ms-blob-type': 'BlockBlob',
                          'x-ms-request-server-encrypted': 'true', 'x-ms-server-encrypted': 'true'}
                common.update(headers or {})
                for name, value in common.items():
                    self.send_header(name, value)
                self.end_headers()
                if self.command != 'HEAD':
                    self.wfile.write(body)

            def error(self, status, code):
                if self.command == 'HEAD':
                    return self.respond(status, headers={'x-ms-error-code': code})
                self.respond(status, json.dumps({'error': {'code': code, 'message': code}}).encode(),
                             {'x-ms-error-code': code, 'Content-Type': 'application/json'})

            def handle_request(self):
                parsed = urllib.parse.urlsplit(self.path)
                valid = self.authenticate(parsed)
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                request = dict(method=self.command, path=self.path, valid_signature=valid, body_length=len(body))
                with owner.lock:
                    owner.requests.append(request)
                    if not valid:
                        return self.error(403, 'AuthenticationFailed')
                    prefix = f'/{ACCOUNT}/fixture'
                    if parsed.path != prefix and not parsed.path.startswith(prefix + '/'):
                        return self.error(404, 'FilesystemNotFound')
                    path = urllib.parse.unquote(parsed.path[len(prefix):].lstrip('/')).rstrip('/')
                    query = {k: v[-1] for k, v in urllib.parse.parse_qs(parsed.query, keep_blank_values=True).items()}
                    if self.command == 'GET' and query.get('resource') == 'filesystem':
                        directory = query.get('directory', '')
                        if directory not in owner.entries or owner.entries[directory] is not None:
                            return self.error(404, 'PathNotFound')
                        prefix = directory + '/' if directory else ''
                        names = [name for name in sorted(owner.entries) if name and name.startswith(prefix)
                                 and (query.get('recursive') == 'true' or '/' not in name[len(prefix):])]
                        offset = int(query.get('continuation') or '0')
                        page = names[offset:offset + 2]
                        payload = {'paths': [dict(name=name, isDirectory=owner.entries[name] is None,
                                                  contentLength=str(len(owner.entries[name] or b'')),
                                                  lastModified=DATE, etag='fixture-etag', owner='fixture',
                                                  group='fixture', permissions='rwxrwxrwx') for name in page]}
                        headers = {'Content-Type': 'application/json'}
                        if offset + 2 < len(names):
                            headers['x-ms-continuation'] = str(offset + 2)
                        return self.respond(200, json.dumps(payload).encode(), headers)
                    if self.command == 'PUT' and query.get('resource') in ['file', 'directory']:
                        parent = path.rsplit('/', 1)[0] if '/' in path else ''
                        if parent not in owner.entries or owner.entries[parent] is not None:
                            return self.error(404, 'ParentNotFound')
                        owner.entries[path] = None if query['resource'] == 'directory' else b''
                        owner.pending.pop(path, None)
                        return self.respond(201)
                    if path not in owner.entries:
                        return self.error(404, 'PathNotFound')
                    data = owner.entries[path]
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
                        staged = owner.pending.setdefault(path, bytearray(data or b''))
                        position = int(query['position'])
                        if position != len(staged):
                            return self.error(400, 'InvalidAppendOperation')
                        staged.extend(body)
                        return self.respond(202)
                    if self.command == 'PATCH' and query.get('action') == 'flush':
                        staged = owner.pending.get(path, data or b'')
                        position = int(query['position'])
                        if position > len(staged):
                            return self.error(400, 'InvalidFlushPosition')
                        owner.entries[path] = bytes(staged[:position])
                        if query.get('retainUncommittedData') != 'true':
                            owner.pending.pop(path, None)
                        request['closed'] = query.get('close') == 'true'
                        return self.respond(200)
                    if self.command == 'DELETE':
                        children = [name for name in owner.entries if name.startswith(path + '/')]
                        if children and query.get('recursive') != 'true':
                            return self.error(409, 'DirectoryNotEmpty')
                        for name in children + [path]:
                            owner.entries.pop(name, None)
                            owner.pending.pop(name, None)
                        return self.respond(200)
                    return self.error(400, 'UnsupportedFixtureOperation')

            do_GET = do_HEAD = do_PUT = do_PATCH = do_DELETE = handle_request

        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.endpoint = f'http://127.0.0.1:{self.server.server_port}/{ACCOUNT}'
        self.connection = f'DefaultEndpointsProtocol=http;AccountName={ACCOUNT};AccountKey={KEY};BlobEndpoint={self.endpoint};DfsEndpoint={self.endpoint};'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
