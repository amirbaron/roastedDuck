# Owned AWS provider fixtures

These scripts use fake credentials and owned local endpoints. They do not require
an AWS account and do not establish live-cloud acceptance. Original SQL is checked
against `extensions/aws/upstream.json` before execution.

`upstream_identity.py` runs the unchanged web-identity and RDS originals. RDS here
means credential generation, as the original explicitly does not connect to a
database. Inputs: `--slt`, `--shim`, `--oracle`, `--source`, `--extensions`,
`--transport` and `--out`. Put outputs on the external artifact volume.
Use the local-services Python environment, which has cryptography installed.

The pinned C++ SDK uses fixed AWS HTTPS endpoints. On macOS, build the test-only
transport adapter into the external artifact directory:

```sh
clang -dynamiclib -O2 -Wall -Wextra -Wno-deprecated-declarations \
  scripts/aws-tests/oracle_transport.c \
  -framework Security -framework CoreFoundation -framework Network \
  -o "$artifact_dir/oracle_transport.dylib"
scripts/sign.sh "$artifact_dir/oracle_transport.dylib"
```

The runner supplies all adapter configuration only to the C++ oracle child. The
adapter redirects its allowlisted DNS/socket endpoints to loopback and injects
an owned CA into those trust evaluations and the oracle's CA-file read. It retains
signature and hostname verification. It does not change system DNS, trust stores,
keychains or proxy settings, and is never linked into roastedDuck. Rust uses an
SDK endpoint override to the same service handler on loopback HTTP. Recorded
requests verify the STS operation, role/token and SSO account/role/bearer token.
This is provider protocol qualification, not a Rust AWS TLS qualification.

`--engine cpp --transport-negative ca` and `--engine cpp --transport-negative
hostname` prove that the transport fixture rejects invalid trust/identity before
an AWS operation reaches the server. These are deliberate rejection controls;
their original SQL file fails at its positive authentication statement, and the
separate fixture verdict records whether the rejection occurred as expected.

`--sso-control` selects `valid`, `missing`, `expired`, `malformed`, `empty-token`,
`invalid-expiry` or `rejected-token`. These supplemental files do not replace the
original SQL. The cache is under the configured credentials file's directory,
matching the pinned C++ SDK. No home-directory token cache is populated.

The web-identity original hardcodes `/tmp/duckdb-oidc-test-token`. The runner
creates an exclusive temporary symlink to its external fake token and removes
only its own link on exit. It refuses to overwrite an existing path. Extensions
are staged under an owned directory using the actual oracle platform. The
production extension manifest, SQL expectations and user configuration remain
unchanged.

Other cohorts are `sql_control.py` (offline SQL), `network_control.py` (signed S3
requests and changing process credentials), and `upstream_{metadata,data,proxy,
environment}.py`. The original unchanged-credential refresh test has a retained
shared C++/Rust failure; do not suppress it or count it as passing acceptance.
