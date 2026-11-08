# C API and ADBC

The [native SDK](install.md) includes the `duckdb.h` interface and a shared
library named `libduckdb`. Symbol names are retained for compatibility.

```c
#include <stdio.h>
#include "duckdb.h"

int main(void) {
    duckdb_database db;
    duckdb_connection con;
    duckdb_result result;
    if (duckdb_open(NULL, &db) != DuckDBSuccess) return 1;
    if (duckdb_connect(db, &con) != DuckDBSuccess) {
        duckdb_close(&db);
        return 1;
    }
    int status = duckdb_query(con, "SELECT 42", &result);
    if (status == DuckDBSuccess) printf("%lld\n", (long long)duckdb_value_int64(&result, 0, 0));
    duckdb_destroy_result(&result);
    duckdb_disconnect(&con);
    duckdb_close(&db);
    return status == DuckDBSuccess ? 0 : 1;
}
```

Compile against the SDK, using a path without spaces for this example:

```sh
cc example.c -I"$ROASTEDDUCK_SDK/include" -L"$ROASTEDDUCK_SDK/lib" \
  -Wl,-rpath,"$ROASTEDDUCK_SDK/lib" -lduckdb -o example
./example
```

## ADBC

Arrow Database Connectivity exposes a columnar database interface. The
candidate preserves the public ADBC initializer and interface headers, with
64 cases / 3,040 assertions per engine/target in the recorded scoped suite.
The SDK's `adbc/` directory contains the interface header and notices.

The release libraries retain 547 public exports and exclude private testing
hooks. Private C++ engine extensions are not implied by C API compatibility.
