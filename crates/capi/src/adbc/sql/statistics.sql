
		SELECT database_name, schema_name, table_name, estimated_size
		FROM duckdb_tables()
		WHERE NOT internal
			AND database_name LIKE %s
			AND schema_name LIKE %s
			AND table_name LIKE %s
		ORDER BY database_name, schema_name, table_name
	