# Plain HTTP forward proxies and HTTPS CONNECT

An independent proxy that rejects CONNECT exposed a real compatibility gap. C++
passed read/PUT/multipart requests through it; Rust1048 sent CONNECT and failed405.
Those reference and failure receipts are retained in1054. The new HTTP transport
sends absolute-form requests through plain HTTP proxies while HTTPS retains the
existing CONNECT tunnel and OpenSSL TLS connector.

## Transport behavior

`crates/httpfs/src/proxy.rs` connects to the configured proxy for HTTP targets,
rewrites the origin-form request target to its absolute URI, and adds configured
Basic proxy authentication. Host and the signed request headers/body stay intact;
the independent receiver verifies SigV4 and the actual upload bytes. Origin URI
userinfo is excluded from the forwarded request target. Debug output omits proxy
credentials and buffered headers.

Request heads may span transport buffers. Once a head is complete, fixed-length
or chunked framing tracks the body without rewriting or buffering the full upload.
This prevents method-looking payload bytes from being treated as a new request on
a reused connection. Chunk extensions/trailers and arbitrary fragment boundaries
are handled. TransportAdapter retains the existing write deadline across rewritten
head fragments; underlying socket registration keeps cancellation/shutdown intact.
No new dependency, native library, query cache or COPY memory policy is introduced.
The scratch storage is request-head/transport-buffer state, not an object cache.

## Verified current component

- Initial Clippy1055 found an invariant expect and redundant closure. The transport
  holder was simplified;1056 passes without suppressing the warnings.
- Fresh test1057, signed1058/runtime1059 passes both protocol tests: fragmented
  heads plus method-looking body/reuse; chunked body/trailers split at every byte.
- Final strict workspace/all-target1061 and native build1062 pass before shared1063
  and SQL1064 are normally signed and pinned-CMS authenticated.
- C++1060 and Rust1065 pass all28 independent S3/proxy cases,314 status/row records.
  New cases require absolute-form requests and reject CONNECT, covering reads,
  simple/multipart writes, Basic proxy auth and a420022-byte method-looking upload.
- TLS1066 passes31 HuggingFace/TLS cases and293 records through the isolated HTTPS
  CONNECT receiver, including certificate expiry/hostname/trust rejections. Raw
  server-side expected TLS alerts remain in the log; the suite exits successfully.
- Full current C API1067 passes30 files/155 cases/742002 assertions; selected
  image and strict signatures verify. Counts include workload-dependent loops.
- Audit1068/source1069 preserve1469 compiled inputs, receipts and immutable native
  identities. No owned heavy job is expected active after these complete receipts.

Current signed development shared SHA256:
`77194d04d04059c794cb48a92d15d4c050e626eef434ce3184a2d79b3d7e7869`.
This is Mac ARM64 development evidence, not final release/platform acceptance.

## Explicit differences and remaining gates

All314 statuses/result sets match.313 error fields match literally; the prior
body-only ETag case still has one extra Rust SQL line/caret. Its exact raw error
comparison is preserved, not normalized away. Overall HTTP request counts are97
Rust versus73 C++; forward-only cases contribute15 versus11. No caching was added
to force the same request scheduling. These counts are not coupled speed/RSS
acceptance or evidence for restoring C++ caching.

The receiver terminates every request locally and never opens an upstream socket.
It does not establish compatibility with all deployed proxy products/configurations.
HTTPS proxy endpoints, broader credential/configuration/lifecycle behavior,
301 Location handling and the explicit query-context annotation remain to verify.
Final common-source release SQL/shell/C API/ADBC, Python/Go/Swift/platform matrices,
persistence refresh and coupled native TPC-H/ClickBench/outlier gates remain open.
Preserve the accepted lower-memory policy and conditional throttled COPY exception.

Evidence is external under `.context/artifacts/platform-matrix-20261029/`,
principally1054–1070. Earlier S3, encrypted and CSV receipts keep their immutable
images and do not form a combined final same-image release count. The full goal
remains active; Mac Intel/extra clients remain out of scope, native crypto allowed.
