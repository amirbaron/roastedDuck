# Persistent refresh metadata interoperability

Current signed Mac ARM64 development1135/1136 passes two bidirectional persistent-
secret journeys: C++create→Rustrefresh→C++reopen and Rustcreate→C++refresh→Rustreopen.
Six separate native processes use the same private secret file in each direction.
Every read's SigV4 signature is independently checked. The receiver rewrites the
file with fresh config credentials; the original engine then reads fresh state
without rewriting it again. All files remain0600. This is development proof, not
final release or other-platform acceptance.

## A real validation difference found

Initial1131 included use_ssl='false' in REFRESH_INFO MAP(VARCHAR,VARCHAR). Rust
accepted that string by applying SQL casts again. C++ refresh invokes the provider
directly and rejects VARCHAR where its provider expects BOOLEAN. This was a
runtime validation mismatch, not incompatible struct-file decoding. The initial
C++failure and Rust over-permissive success remain preserved.

The engine now validates refreshed BOOLEAN options before calling the config
provider: use_ssl,verify_ssl,url_compatibility_mode,requester_pays. Normal SQL
creation still applies named-parameter casts. The new bad-bool case asserts C++'s
exact invalid-type message;14 independent signature cases pass on both engines.

The positive1132/1138 journeys use C++-valid string refresh options and set transport
booleans in the consumer session. They do not remove the invalid test:1131 remains
its evidence and the live control retains the new14th negative case. Typed
refresh_info structs written by either engine are consumed and refreshed by the
other, preserving provider,name,scope,persistence andstorage.

## Current same-image evidence

- Strict workspace1133 and fresh native1134 succeed before normally signed and
  pinned-CMS authenticated CAPI1135/SQL1136 execute.
-1137:14 independent signed cases on C++934+officialHTTPFS590 and Rust1135,
  including direct-provider boolean rejection.
-1138:two persistent refresh directions/six native stages,source/rewrite/reopen
  file hashes and0600 permissions,actual selected native image per worker.
-1139:36 signed wire controls,31 TLS/293records,35 write functional checks plus
  one16515072EXTENSION allocation sample (zero before/after,notRSS),40 secret-
  transaction cases/644 matching observations on both engines.
-1140:30 public C API files/155 cases/775360 assertions pass, actual
  selected-image and strict-signature postchecks.
-1141:component audit; source1142 archives the current code and controls.

Current signed native SHA256:`635d7229ce479e466d26f3bc0a16e9c842a95ded7b5cb7db0fc2dc81e2217b15`.
All evidence is external under `.context/artifacts/platform-matrix-20261029/`.
The source-built C++1.5.6 comparator retains its recorded dummy build ID; it is not
an official engine distribution. No dependency,cache or COPY-memory policy changed.

Nested refresh_info remains redacted by default rather than reproducing the C++
listing's replacement-secret disclosure. Explicit authorized unredacted listing
works. Cross-origin tokens remain stripped; one extra ETag query-context diagnostic
and request-count differences stay explicit. These are not exact header/diagnostic
parity or coupled performance acceptance claims.

Concurrent refresh/cancellation/disconnect/failed-commit lifecycle and custom
provider/serializer applicability still need verification. Final common-source
signed release whole SQL/shell/C API/ADBC,Python/Go/Swift and required platforms,
persistence refresh and native coupled speed/RSS/outlier acceptance remain open.
No Mac Intel or extra clients required. Full goal active/incomplete.
