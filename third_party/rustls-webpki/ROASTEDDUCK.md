# Local CRL parser patch

Vendored rustls-webpki 0.103.17; the upstream license is unchanged.
Registry archive SHA-256: `e3a0760a887c14823fca5a1c970b01d47c328c696c0c64829d1df50e8f853fd5`.

Only src/crl/types.rs differs: v2 CRLs may omit the optional extensions field,
as accepted by the libpq 18.3 bundled with DuckDB 1.5.6's PostgreSQL extension.
Present extensions still use the existing parser and critical-extension checks.
Signed bytes, signature verification, chain construction, issuer matching,
revocation lookup and expiration checks are unchanged. No dependency is added.
The PostgreSQL fixture covers both extension-free and profile-compliant CRLs.
