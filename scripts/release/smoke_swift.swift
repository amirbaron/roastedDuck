import DuckDB

let database = try Database(store: .inMemory)
let connection = try database.connect()
let result = try connection.query("SELECT 42 AS answer")
precondition(result.rowCount == 1)
precondition(result[0].cast(to: Int32.self)[0] == 42)
print("Swift preview smoke passed")
