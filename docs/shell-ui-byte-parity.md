# Shell color and pager byte boundaries

The settings/numeric checkpoint leaves legacy text callbacks for color selection and
pager configuration. Their argv reaches the native dispatcher but still converts to
String before the callback. Highlight errors and deprecated color-command guidance
interpolate those strings; pager commands also remain String through reporting and
process launch. These are source-audit findings, not new runtime results.

Before changing these paths, compare signed CLI6 with frozen C++ 1.5.6 using actual
byte arguments and raw stdout/stderr/exit/file effects. Cover highlight element,
color and intensity arguments; six deprecated color commands; pager command/report
and row-threshold parsing. Include ASCII/UTF8 controls and malformed-byte classes,
then all non-NUL byte suffixes where useful. Do not execute a supplied pager command
in the configuration probe. Keep crashes, fixture errors and raw trace differences
open. Preserve prior signed images and evidence under the external SSD artifact task.

Retain ASCII case matching, native byte-based candidate ranking, existing validation
priority and arbitrary pager command bytes. Keep valid-string callers working.
Prompt parsing/rendering and other legacy callbacks remain separate open boundaries;
this probe does not establish interactive pager execution or whole-shell completion.
Run one build/test workload at a time, sign every new executable, and preserve the
accepted lower-memory engine policy. Shared Python rendering code is outside this
CLI-only scope; any scope change must trigger corresponding consumer verification.

## Measured baseline

The unchanged exact-byte recorder measures110cases on signed CLI6:60exact/50raw
mismatches, no crashes/timeouts/fixture errors. All11ASCII and11UTF8controls match.
Each of five malformed payload classes differs in highlight element/color/intensity,
six deprecated color messages and pager command reporting. Pager row-threshold
parsing already matches. Evidence: `shell-argv-20261025/ui-baseline-1.jsonl` and its
manifest/summary; no pager process is launched by these configuration journeys.

The pager carrier reaches both OS environment defaults and actual process launch.
Add bounded real PTY journeys with a safe printf-marker/cat pager: configure native
bytes via -cmd or each environment default, verify the marker bytes, query output,
return to normal stdout after .pager off, and exact C++ terminal/file effects.
Keep these separate from the110caseconfiguration baseline and preserve raw output.

## Native implementation and verification

Color callbacks now preserve bytes through element/color/intensity selection, candidate
ranking, errors and deprecated guidance. Pager command storage/reporting, environment
defaults and the OS process argument retain native bytes. ASCII matching and validation
order are unchanged; numeric threshold parsing keeps its existing native algorithm.
The changed source is limited to CLI state/metadata/highlight modules, outside the four
shared rendering-library modules verified in Python wheel4.

Signed CLI7 closes all50baseline mismatches:110/110exact, zero earlier matches regressed.
The exhaustive matrix is2,915/2,915exact with no fixture errors, crashes, timeouts or
waivers. Real PTY journeys improve6/12to12/12exact: command arguments and both pager
environment defaults each match with ASCII/UTF8/invalidFF/incompleteC3 payloads.
Actual printf marker bytes, pager query output and normal query output after pager off
match, including exit0 and raw terminal/file effects. No normalization is used.

The first PTY attempt omitted the explicit terminal theme flag and stalled both engines,
including ASCII controls, at OSC theme detection. It was stopped; partial raw evidence
and `pager-probe-invalid-1.json` remain. It is invalid harness evidence, not an engine
failure or parity result. The corrected harness uses the existing suite's dark-mode
flag, suppresses the banner and requires valid C++ marker/exit controls.

Workspace/all-target checking and warnings-denied Clippy pass. Signed shell units pass15;
upstream shell tests pass392with3existing skips. All13affected color/pager differential
cases pass, including4PTYsessions, with no known markers. The full768case/46PTYrun
remains CLI6 evidence; it is not relabeled as a CLI7 run. CLI7 signed SHA:
aed606af9c1b1dfeb4ef0237bd721fb245550ab1fd0083cecde56446f3609aa4.

Evidence: `.context/artifacts/shell-argv-20261025/ui-verification-1.json`, source/image
manifests, `ui-{baseline,after,exhaustive}-1.jsonl`, `pager-baseline-2.jsonl`,
`pager-after-1.jsonl`, raw test logs/JUnit and preserved probe sources. Prompt/UI byte
boundaries, public catalog IDs, raw Stack Trace formatting, broader core/client/storage
and final coupled speed-memory/per-query acceptance remain open. No table cache changed.
