# Rosetta connection warning and terminal output

The signed C++ Intel comparator exposed a missing connection diagnostic. For the
same `SELECT 42` CLI arguments under Rosetta, C++ returned278 stdout bytes including
its warning while Rust returned only the three-byte result. This was preserved
before changing the implementation.

Source33 adds a read-only Apple Intel`sysctl.proc_translated` probe and emits the
exact pinned C++ message through the existing connection logger when warnings are
enabled. Native arm64 and non-Apple builds take the false path. No direct console
printing, new dependency, data cache or retained memory is introduced.

The signed refreshed artifacts pass Clippy, four integer-average regressions and
three instance-cache regressions under Rosetta. Three independent C++/Rust CLI
comparisons match stdout, stderr and exit code exactly, including the complete
colored warning, UTF-8/extended-precision values and multi-query output.

## Terminal buffering correction

The full differential then exposed eleven PTY mismatches: Rust flushed a trailing
ANSI reset before the startup terminal query, while C++ retained it in line-buffered
stdout. Source35 removes the unconditional terminal flush after each write. Partial
lines now wait for the next newline or explicit flush, matching C stdio.

The existing filtered differential contains all46 PTY cases plus30 incidental
empty-output cases. All76 match exactly with the corrected signed shell; no
normalization or new known-difference marker was added. The three batch comparisons
also remain exact matches. The refreshed paired C API harness passes all30 files/
155 cases on each engine. The completed full shell rerun is classified below.

## Preserved upstream and metadata outcomes

The unmodified upstream shell suite under Rosetta produces identical outcomes on
both source33 engines:387 pass,three shared skips and five shared failures. The
warning adds output to tests expecting empty stdout or a different warning count.
These remain failures in the reports, not rewritten passes.

The first768-case differential recorded fifteen differences plus one existing
implementation-stack-frame exception. Eleven were the corrected terminal reset
ordering. Four show build-source identity: the C++ comparator's documented
synthetic`0123456789`versus the Rust build's reported source ID. Those metadata
differences remain explicit; neither identity is changed to manufacture equality.
The refreshed complete differential now records exactly those four metadata cases
plus the existing known native-stack-frame exception. All eleven PTY mismatches
are gone. The original nonzero differential exit is retained; it is not described
as zero byte differences or a wholly passing upstream suite.

## Artifact and fixture scope

The current shell lives in`work/apple/macos-intel-native-signed-3/`. Its core library,
SQL runner and regression executables remain byte-identical to signed source33
payloads in set2; the source35 shell is replaced and signed, then the entire set's
manifest is authenticated. Signature/payload checks pass before and after focused
runtime comparisons. All execution is x86_64 under Rosetta on an arm64 host.

An initial reconstruction overlay omitted the newly modified printer file because
it had not previously been in the changed-file set. Source35 adds it explicitly
and a fresh guarded Cargo validation succeeds before acceptance. No input guard is
weakened. A separate combined CLI fixture initially tried to parse the trailing
ANSI reset as part of42; raw C++/Rust bytes already matched. Its fresh replacement
compares complete bytes first, then removes style codes only for numeric CSV parsing.
Both failed fixture/provenance attempts are preserved.

## Evidence

Dedicated SSD root:`.context/artifacts/platform-matrix-20261029/`.

- `work/apple/rosetta-warning-before-1/`: original exact output mismatch.
- `source-33/`: platform probe/logger change; `source-35/`: complete printer fix overlay.
- `macos-intel-native-build-2/`, `macos-intel-native-freeze-2.json`: complete release
  build/signing and source33 payload identities.
- `work/apple/macos-intel-sdk-run-3/verification.json`: seven tests and exact CLI bytes.
- `macos-intel-shell-build-4/`, `macos-intel-native-freeze-3.json`: corrected shell's
  guarded build and authenticated artifact set.
- `work/apple/rosetta-warning-after-{1,2}/verification.json`: exact batch comparisons
  before and after the terminal-buffering correction.
- `work/apple/pty-line-buffer-1/verification.json`:0/76 differences, including all46 PTYs.
- `work/apple/macos-intel-shell-1/verification.json`: original complete upstream and
  differential outcomes, including all shared failures and metadata differences.
- `work/apple/macos-intel-shell-2/verification.json`: refreshed complete upstream
  outcomes and768-case differential; four explicit metadata differences remain.
- `work/apple/macos-intel-capi-2/verification.json`: all155 cases on both engines
  after the Rosetta warning/core refresh.

No native Intel-hardware performance or final integrated release acceptance follows
from these Rosetta correctness results. Other platform/client/storage/performance
gates remain open.
