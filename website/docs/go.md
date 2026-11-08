# Go

The preview uses the pinned upstream Go client against roastedDuck's native
library. Its default bundled C++ engine is outside this integration contract.

[Download and extract the native SDK](install.md), then set `ROASTEDDUCK_SDK`
to its absolute directory. The SDK includes the pinned `go/duckdb-go` sources.
Use Go 1.24 or newer, a C compiler, and CGO.

```sh
mkdir roastedduck-example
cd roastedduck-example
go mod init example.com/roastedduck-example
go mod edit -require=github.com/duckdb/duckdb-go/v2@v2.10506.0
go mod edit -replace="github.com/duckdb/duckdb-go/v2=$ROASTEDDUCK_SDK/go/duckdb-go"
```

Create `main.go`:

```go
package main

import (
    "database/sql"
    "fmt"
    "log"

    _ "github.com/duckdb/duckdb-go/v2"
)

func main() {
    db, err := sql.Open("duckdb", "")
    if err != nil { log.Fatal(err) }
    defer db.Close()
    var answer int
    if err := db.QueryRow("SELECT 42").Scan(&answer); err != nil { log.Fatal(err) }
    fmt.Println(answer)
}
```

Build and run using both required tags:

```sh
go mod tidy
export CGO_ENABLED=1
export CGO_CFLAGS="-I$ROASTEDDUCK_SDK/include"
export CGO_LDFLAGS="-L$ROASTEDDUCK_SDK/lib -Wl,-rpath,$ROASTEDDUCK_SDK/lib"
go run -tags duckdb_use_lib,duckdb_arrow .
```

Use an SDK path without spaces for this simple compiler-flag example. The
dynamic loader must select the SDK's `libduckdb`, not another installed engine.
The recorded candidate suite covers 652 named cases plus typed persistence
journeys on each desktop target. See [compatibility](compatibility.md).
