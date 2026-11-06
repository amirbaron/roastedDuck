# Encrypted database and WAL cross-engine compatibility

Current signed Mac ARM64 development1031 interoperates with native source-built
C++ DuckDB1.5.6 release934 for both supported ciphers and encryption versions:
GCM/v0, GCM/v1, CTR/v0 and CTR/v1. The final authenticated matrix1041 passes16
bidirectional persistence journeys and32 incomplete-tail recovery checks across88
native process stages. This is current development evidence, not final release or
other-platform acceptance.

## Repeatable harness

`scripts/storage-tests/encrypted_compat.py` accepts signed Rust, C++ and HTTPFS
bundle directories, an output directory and a mandatory certificate SHA256 pin.
Use `--cipher GCM|CTR` and `--encryption-version v0|v1`. Run each profile serially;
large outputs belong on the dedicated external artifact directory. The helper
`encrypted_worker.py` uses the existing public C API control, with no new dependencies.

The runner verifies all three pinned CMS manifests before native execution and
afterward, explicitly verifies that the selected native files are listed with the
correct hash, and applies strict normal codesign checks to Mac engine libraries.
The signed HTTPFS package is verified as a whole, including its required trailer;
its whole-file normal codesign limitation is retained from earlier provenance.
Each worker records its actual loaded engine image and rejects a second DuckDB
engine image. Linux has a /proc image check, but that branch has not been runtime
verified by this Mac component. No user extension or secret directory is used.

Before hard exit, creators commit writes and check their typed data. They then use
os._exit to preserve a genuine nonempty WAL instead of letting shutdown checkpoint
it. The parent requires a nonempty WAL for WAL cases and zero/no WAL after explicit
checkpoint cases. Each engine runs in a separate process.

## Journeys and checked values

For every cipher/version combination:

1. C++ creates checkpointed encrypted data; Rust reads, appends, updates and
   checkpoints it; C++ reopens and verifies the receiver's writes.
2. C++ creates a checkpoint plus committed nonempty encrypted WAL; Rust replays,
   verifies and mutates it, checkpoints, and C++ reads the resulting data back.
3. The same checkpoint journey runs Rust→C++→Rust.
4. The same nonempty-WAL journey runs Rust→C++→Rust.

Expected values are independent Python literals, not copied from either engine.
The fixture includes a primary key, persisted text index, long Unicode strings,
DECIMAL(38,9), UUID, binary NUL/FF bytes, microsecond timestamps, lists and NULLs.
Readback checks include indexed text predicates. Every noncreator worker also
checks wrong-key rejection before opening with the correct key and cipher.

Two additional sealed WAL fixtures, one from each producer, supply independent
copies to both readers. The parent hashes each input and proves the C++/Rust
copies are identical and the sealed source remains unchanged:

| Incomplete encrypted WAL tail | C++ and Rust result in all four profiles |
| --- | --- |
| Append one incomplete byte | All committed inserts and updates survive |
| Remove the final byte | The earlier insert survives; the last update is ignored |

Each recovery compares the full checked typed state, including the original
checkpoint, rather than accepting successful open alone. Both readers agree with
the independent expected state. Raw file hashes, SQL, process exits, actual loaded
images and authentication logs are preserved per stage.

## Evidence and limits

The dedicated external directory is `.context/artifacts/platform-matrix-20261029/`:

- 1037: initial four default-format persistence journeys,12 native stages.
- 1038: initial eight incomplete-tail recovery processes on identical input copies.
- 1039: first run of the repository's reusable22-stage harness.
- 1040: first complete four-profile matrix; original harness source is retained there.
- 1041: final four-profile matrix with explicit selected-file manifest membership
  and strict Mac engine signatures;16 journeys/32 recovery checks/88 stages pass.
- 1042: final component audit;56 physical database headers confirm the encrypted
  flag, cipher byte and version byte against the C++ MainHeader serialization.
  Source1043 includes the reusable harness and docs.

Do not add historical prototype counts to the final matrix or silently replace
failed/canonical corpus verdicts. The engine code is unchanged from source1036;
only reusable verification scripts and documentation are added. Native SHA256:
`a01119542ce034f09e76d532f59d271d305b89b766101e4654eee2960965bbea`.
The C++ comparator is source-built1.5.6 with its recorded dummy build ID, not an
official distributed binary. The HTTPFS package is the authenticated official590
package selecting the Rust provider when loaded by the port.

The matrix covers two specific incomplete-tail conditions, not arbitrary corruption
or power-loss locations. These timings are functional-test observations, not
coupled performance measurements. No cache, memory throttle or engine code changed.
The accepted lower-memory policy and conditional COPY exception remain intact.

The full goal remains active: remaining core/HTTPFS lifecycle/redirect/version/
proxy behavior, final common-source signed release whole SQL/shell/C API/ADBC,
Python/Go/Swift and required platform matrices, and native coupled TPC-H/ClickBench
speed/RSS/outlier acceptance still need verification. Repeat these encrypted journeys
on the final relevant artifacts. Mac Intel and extra clients remain outside scope;
third-party native crypto is explicitly allowed while the DuckDB engine remains Rust.
