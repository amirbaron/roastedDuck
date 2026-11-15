"""Owned Azure account namespaces and a restricted loopback CONNECT proxy."""
import base64
import email.utils
import hashlib
import http.server
import json
import selectors
import socket
import threading
import time
import traceback
import urllib.parse
import xml.etree.ElementTree as ET

from dfs_fixture import DATE, KEY
from identity_control import IdentityFixture

BLOB_ACCOUNT = 'blobfixture'
DFS_ACCOUNT = 'dfsfixture'
PUBLIC_ACCOUNT = 'publicfixture'
HOSTS = [f'{account}.{service}.core.windows.net'
         for account in [BLOB_ACCOUNT, DFS_ACCOUNT, PUBLIC_ACCOUNT] for service in ['blob', 'dfs']]


class ConnectProxy:
    def __init__(self, target_port, port=0, authenticated=False):
        self.target_port = target_port
        self.port = port
        self.authenticated = authenticated
        self.requests = []
        self.sockets = set()
        self.lock = threading.Lock()
        self.stopped = threading.Event()
        self.errors=[]
        self.error_count=0

    def __enter__(self):
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_CONNECT(self):
                host, port = self.path.rsplit(':', 1)
                allowed = (host in HOSTS and port == '443') or (host == '127.0.0.1' and int(port) == owner.target_port)
                authorized = not owner.authenticated or self.headers.get('Proxy-Authorization') == 'Basic ' + base64.b64encode(b'john:doe').decode()
                owner.requests.append(dict(authority=self.path, authorized=authorized, forwarded=allowed and authorized))
                if not authorized or not allowed:
                    self.send_response(407 if not authorized else 403)
                    self.send_header('Proxy-Authenticate', 'Basic realm="local fixture"')
                    self.send_header('Content-Length', '0')
                    self.end_headers()
                    return
                try:
                    upstream = socket.create_connection(('127.0.0.1', owner.target_port), timeout=10)
                except OSError as error:
                    owner.error_count += 1
                    if len(owner.errors)<20:
                        owner.errors.append(str(error))
                    self.send_error(502)
                    return
                with owner.lock:
                    owner.sockets.update([upstream, self.connection])
                try:
                    self.send_response(200, 'Connection established')
                    self.end_headers()
                    self.wfile.flush()
                    with selectors.DefaultSelector() as selector:
                        selector.register(self.connection, selectors.EVENT_READ, upstream)
                        selector.register(upstream, selectors.EVENT_READ, self.connection)
                        while not owner.stopped.is_set():
                            for key, _ in selector.select(0.2):
                                data = key.fileobj.recv(65536)
                                if not data:
                                    return
                                key.data.sendall(data)
                except (OSError, ConnectionError):
                    pass
                finally:
                    with owner.lock:
                        owner.sockets.difference_update([upstream, self.connection])
                    upstream.close()

        class Server(http.server.ThreadingHTTPServer):
            request_queue_size=256

            def handle_error(self,request,address):
                owner.error_count+=1
                if len(owner.errors)<20:
                    owner.errors.append(traceback.format_exc()[-5000:])

        self.server = Server(('127.0.0.1', self.port), Handler)
        self.url = f'http://127.0.0.1:{self.server.server_port}'
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.stopped.set()
        self.server.shutdown()
        self.server.server_close()
        with self.lock:
            for connection in self.sockets:
                connection.close()
        self.thread.join()


def directories(entries, path):
    pieces = path.split('/')[:-1]
    for count in range(1, len(pieces) + 1):
        entries['/'.join(pieces[:count])] = None


class CloudFixture(IdentityFixture):
    def __init__(self, context, certificate, data):
        super().__init__(context, certificate)
        self.signing_account = BLOB_ACCOUNT
        # Service default. Small-page continuation coverage stays in dfs_control.
        self.list_page_size = 5000
        self.accounts = {}
        self.staged_blocks = {}
        self.storage_requests = []
        self.versions={}
        self.modified={}
        dataset = {file.relative_to(data).as_posix(): file.read_bytes()
                   for file in data.rglob('*') if file.is_file()}
        for account in [BLOB_ACCOUNT, DFS_ACCOUNT, PUBLIC_ACCOUNT]:
            containers = self.accounts[account] = {}
            for container in ['duckdblabs-data', 'testing-public', 'testing-private', 'duckdblabs-write-testing', 'writes']:
                entries = {'': None}
                if container == 'duckdblabs-data':
                    entries.update({'common/azure_data/' + key: value for key, value in dataset.items()})
                elif container in ['testing-public', 'testing-private']:
                    entries.update(dataset)
                elif container == 'duckdblabs-write-testing':
                    entries.update({'extension': None, 'extension/azure': None, 'extension/azure/many': None, 'extension/azure/big': None})
                for path in list(entries):
                    directories(entries, path)
                containers[container] = (entries, {})
                for path in entries:
                    key=(account,container,path)
                    self.versions[key]=0
                    relative=path.removeprefix('common/azure_data/') if container=='duckdblabs-data' else path
                    source=data/relative
                    self.modified[key]=source.stat().st_mtime if source.exists() else data.stat().st_mtime

    def __enter__(self):
        super().__enter__()
        self.connection = f'DefaultEndpointsProtocol=https;AccountName={BLOB_ACCOUNT};AccountKey={KEY};BlobEndpoint=https://{BLOB_ACCOUNT}.blob.core.windows.net;DfsEndpoint=https://{BLOB_ACCOUNT}.dfs.core.windows.net;'
        return self

    def handle_identity(self, handler, parsed, body):
        if handler.headers.get('Host', '').split(':')[0] in HOSTS:
            return False
        return super().handle_identity(handler, parsed, body)

    def location(self, handler, parsed):
        account = handler.headers.get('Host', '').split('.')[0]
        container, _, path = urllib.parse.unquote(parsed.path).lstrip('/').partition('/')
        return account, container, path.rstrip('/')

    def locate(self, handler, parsed):
        account, container, path = self.location(handler, parsed)
        storage = self.accounts.get(account, {}).get(container)
        if storage is None:
            return None
        return storage[0], storage[1], path

    def allow_anonymous(self, handler, parsed):
        account, container, _ = self.location(handler, parsed)
        return account == PUBLIC_ACCOUNT and container == 'testing-public'

    def error_reason(self, code):
        return {'ContainerNotFound':'The specified container does not exist.',
                'BlobNotFound':'The specified blob does not exist.',
                'PathNotFound':'The specified file does not exist.'}.get(code)

    def current_etag(self,handler,parsed):
        key=self.location(handler,parsed)
        digest=hashlib.sha256('/'.join(key).encode()).hexdigest()[:20]
        return f'"{digest}-{self.versions.get(key,0)}"'

    def response_headers(self,handler,status):
        if handler.headers.get('Host','').split(':')[0] not in HOSTS or not 200<=status<300:
            return {}
        parsed=urllib.parse.urlsplit(handler.path)
        key=self.location(handler,parsed)
        query={k:v[-1] for k,v in urllib.parse.parse_qs(parsed.query).items()}
        mutates=(handler.command=='PUT' and (query.get('comp')=='blocklist' or query.get('resource') in ['file','directory'])) or (handler.command=='PATCH' and query.get('action')=='flush')
        if mutates:
            self.versions[key]=self.versions.get(key,0)+1
            self.modified[key]=time.time()
        return {'ETag':self.current_etag(handler,parsed),
                'Last-Modified':email.utils.formatdate(self.modified.get(key,time.time()-86400),usegmt=True)}

    def path_properties(self,handler,name):
        account,container,_=self.location(handler,urllib.parse.urlsplit(handler.path))
        key=(account,container,name)
        digest=hashlib.sha256('/'.join(key).encode()).hexdigest()[:20]
        return dict(lastModified=email.utils.formatdate(self.modified.get(key,time.time()-86400),usegmt=True),
                    etag=f'{digest}-{self.versions.get(key,0)}')

    def handle_storage(self, handler, parsed, body, entries, pending, path):
        account, container, _ = self.location(handler, parsed)
        query = {k:v[-1] for k,v in urllib.parse.parse_qs(parsed.query, keep_blank_values=True).items()}
        self.storage_requests.append(dict(account=account, host=handler.headers.get('Host'), container=container, method=handler.command,
                                         path=path, query=query, body_length=len(body)))
        blob = '.blob.' in handler.headers.get('Host', '')
        if handler.command in ['HEAD', 'GET'] and not query and (
                path not in entries or (blob and account != DFS_ACCOUNT and entries[path] is None)):
            handler.error(404, 'BlobNotFound' if blob else 'PathNotFound')
            return True
        if handler.command == 'GET' and query.get('comp') == 'list':
            prefix = query.get('prefix', '')
            names = [name for name in sorted(entries) if entries[name] is not None and name.startswith(prefix)]
            start = int(query.get('marker') or '0')
            page_size = min(self.list_page_size, int(query.get('maxresults') or self.list_page_size))
            page = names[start:start + page_size]
            root = ET.Element('EnumerationResults', ServiceEndpoint=f'https://{account}.blob.core.windows.net/', ContainerName=container)
            blobs = ET.SubElement(root, 'Blobs')
            for name in page:
                item = ET.SubElement(blobs, 'Blob')
                ET.SubElement(item, 'Name').text = name
                properties = ET.SubElement(item, 'Properties')
                metadata=self.path_properties(handler,name)
                for field, value in [('Creation-Time', metadata['lastModified']), ('Last-Modified', metadata['lastModified']), ('Etag', '"'+metadata['etag']+'"'),
                                     ('Content-Length', str(len(entries[name]))), ('Content-Type', 'application/octet-stream'),
                                     ('BlobType','BlockBlob'), ('LeaseStatus','unlocked'), ('LeaseState','available')]:
                    ET.SubElement(properties, field).text = value
            ET.SubElement(root, 'NextMarker').text = str(start + page_size) if start + page_size < len(names) else ''
            handler.respond(200, ET.tostring(root), {'Content-Type':'application/xml'})
            return True
        block_key = (account, container, path)
        if handler.command == 'PUT' and query.get('comp') == 'block':
            self.staged_blocks.setdefault(block_key, {})[query['blockid']] = body
            handler.respond(201)
            return True
        if handler.command == 'PUT' and query.get('comp') == 'blocklist':
            if not body:
                handler.error(400, 'InvalidXmlDocument')
                return True
            identifiers = [node.text for node in ET.fromstring(body)]
            blocks = self.staged_blocks.setdefault(block_key, {})
            if any(key not in blocks for key in identifiers):
                handler.error(400, 'InvalidBlockList')
            else:
                entries[path] = b''.join(blocks[key] for key in identifiers)
                directories(entries, path)
                handler.respond(201)
            return True
        return False
