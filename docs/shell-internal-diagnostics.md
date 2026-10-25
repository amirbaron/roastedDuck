# Internal shell diagnostics

The current open-errors control matches CPPstdout, exit1 and the non-trace diagnostic
prefix/footer. CPPprints one native Stack Trace section; Rust omits it. Repeating CPP
against itself is byte-stable for this binary: the symbol names and instruction offsets
are build-specific, not evidence of runtime randomness. Raw controls are preserved in
`.context/artifacts/shell-final-gaps-20261025/open-error-controls-1.json` and stream files.

Frozen ErrorData::ConstructFinalMessage appends the standard internal-error explanation
and, when captured pointers exist, resolves the originating native trace. Rust already
matches the explanation. Add opt-in originating trace capture for the shell's missing
connection invariant and render a nonempty stack_trace extra-info field after that
explanation. Keep ordinary error construction/formatting, query values, state/exit behavior
and successful-query memory policy unchanged. Use actual Rust frames, not fabricated CPP
symbols or offsets; retain both raw streams and report any remaining frame difference.

Verify the actual null-connection journey, its stdout/exit/file effects and diagnostic
prefix/footer/trace presence. Trace rendering must preserve native error bytes and
cloned error data. Existing plain internal errors should remain unchanged without a
captured trace. Run appropriate signed common/shell tests and the full shell cohorts.
Do not remove the known difference merely by masking arbitrary trailing diagnostics.
The catalog OID difference and broader client/performance gates remain separate.

## Originating trace verified

Signed common31/31tests pass. A fresh signed CLI reproduces the full failing .open
sequence with exact stdout/exit1/file effects and byte-identical non-trace diagnostics.
Both engines now emit exactly one nonempty Stack Trace section. The Rust body includes
the actual shell call site and source lines after returning from the error constructor;
CPPretains its native symbols/offsets. Raw bodies differ and remain explicitly recorded.
Receipt: shell-final-gaps-20261025/trace-current-1.json and unmodified stream files.

Tighten this case's known-difference handling to accept only the native-frame body:
require exact other result fields and stderr prefix/footer, exactly one trace marker and
nonempty trace bodies. Missing traces or changed diagnostics/exit/files must fail. Keep
the raw frame difference classified explicitly; do not fake CPPframes or normalize whole
trailing error streams. Exercise negative controls against the checker, then run the
full shell suites. OID numbering remains open and separate.


## Full shell verification

The new signed CLI passes all392upstream shell tests with the same3skips and all768
expanded differential cases/46PTY with0unexpected differences. Two explicit differences
remain: native implementation/compiler frame content (now guarded to frame body only),
and catalog OID numbering. The missing section is fixed; raw frame identity is not claimed.
All7comparator negative controls fail as intended. Native signatures/source/binary hashes
remain unchanged after execution. Appropriate common/shell Clippy is running separately.

Evidence: `.context/artifacts/shell-final-gaps-20261025/verification-1.json`,
`common-test-1`, `signed-shell-1`, `shell-upstream-trace-1`, `shell-diff-trace-1`,
trace-current-1 raw streams and comparator controls. Earlier full core/client acceptance
is on its recorded pre-trace images; this trace change is opt-in at the shell invariant.
Other internal-diagnostic surfaces, OID lifecycle parity, broader client/platform/storage
and final coupled performance acceptance remain open.


Common/shell release all-target Clippy now passes with warnings denied. Its first run
found only assert_is_empty in the new unit test; the assertion is rewritten as suggested,
with no production change. Receipt verification-2.json preserves both the executed binary/
source checkpoint and that test-only follow-up. Next investigate OID allocation through
public multi-instance/catalog lifecycle controls before changing catalog implementation.
