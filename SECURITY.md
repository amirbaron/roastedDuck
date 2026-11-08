# Security policy

roastedDuck is a public preview. Only the latest preview receives fixes;
there is no long-term support commitment yet.

Please report vulnerabilities through
[GitHub private vulnerability reporting](https://github.com/amirbaron/roastedDuck/security/advisories/new).
Include the release/commit, operating system, a minimal reproducer, and the
observed impact. Do not disclose credentials, sensitive databases, or active
exploitation details in a public issue.

roastedDuck runs in the embedding process. SQL can read or write files and,
when enabled, access network resources. Do not treat arbitrary SQL as a sandbox.
Apply the same operating-system and application-level isolation you would use
for any embedded database that processes untrusted input.

Rust implementation does not imply that the entire dependency graph is Rust,
that unsafe code is absent, or that a security audit has been completed.
