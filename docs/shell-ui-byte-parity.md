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
