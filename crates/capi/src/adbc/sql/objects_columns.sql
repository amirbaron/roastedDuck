
				WITH columns AS (
					SELECT
						table_catalog,
						table_schema,
						table_name,
						LIST({
							column_name: column_name,
							ordinal_position: ordinal_position,
							remarks: comment,
							xdbc_data_type: NULL::SMALLINT, -- Arrow type ID not derivable from SQL; SQL type codes are in xdbc_sql_data_type
							xdbc_type_name: data_type,
							xdbc_column_size: CASE
								WHEN base_type = 'DATE' THEN 10::INTEGER
								WHEN data_type IN ('TIME', 'TIME WITH TIME ZONE', 'TIME_NS') THEN 15::INTEGER
								WHEN data_type LIKE 'TIMESTAMP%%' THEN 26::INTEGER
								ELSE numeric_precision::INTEGER
							END,
							xdbc_decimal_digits: numeric_scale::SMALLINT,
							xdbc_num_prec_radix: numeric_precision_radix::SMALLINT,
							xdbc_nullable: CASE is_nullable
								WHEN FALSE THEN 0::SMALLINT
								WHEN TRUE THEN 1::SMALLINT
								ELSE 2::SMALLINT
							END,
							xdbc_column_def: column_default,
							xdbc_sql_data_type: CASE
								WHEN data_type = 'TIMESTAMP WITH TIME ZONE' THEN 2014::SMALLINT
								WHEN data_type LIKE 'TIMESTAMP%%' THEN 93::SMALLINT
								WHEN data_type = 'TIME WITH TIME ZONE' THEN 2013::SMALLINT
								WHEN data_type LIKE '%%]' THEN 2003::SMALLINT
								WHEN type_codes[base_type] IS NOT NULL THEN type_codes[base_type]::SMALLINT
								ELSE 1111::SMALLINT  -- Types.OTHER: aligned with DuckDB JDBC default for unmapped types
							END,
							xdbc_datetime_sub: CASE
								WHEN base_type = 'DATE' THEN 1::SMALLINT
								WHEN data_type LIKE 'TIMESTAMP%%' THEN 3::SMALLINT
								WHEN data_type IN ('TIME', 'TIME WITH TIME ZONE', 'TIME_NS') THEN 2::SMALLINT
								ELSE NULL::SMALLINT
							END,
							xdbc_char_octet_length: CASE
								WHEN base_type IN ('VARCHAR', 'BLOB') THEN character_maximum_length::INTEGER
								ELSE NULL::INTEGER
							END,
							xdbc_is_nullable: CASE is_nullable
								WHEN FALSE THEN 'NO'
								WHEN TRUE THEN 'YES'
								ELSE ''
							END,
							xdbc_scope_catalog: NULL::VARCHAR,  -- REF types not supported in DuckDB
							xdbc_scope_schema: NULL::VARCHAR,
							xdbc_scope_table: NULL::VARCHAR,
							xdbc_is_autoincrement: NULL::BOOLEAN,    -- not exposed via duckdb_columns()
							xdbc_is_generatedcolumn: NULL::BOOLEAN,  -- not exposed via duckdb_columns()
						}) table_columns
					FROM (
						SELECT
							database_name AS table_catalog,
							schema_name AS table_schema,
							table_name,
							column_name,
							column_index AS ordinal_position,
							comment,
							column_default,
							is_nullable,
							numeric_scale,
							numeric_precision,
							numeric_precision_radix,
							character_maximum_length,
							data_type,
							STRING_SPLIT(data_type, '(')[1] AS base_type, -- normalize typemods for type-code lookup
							-- JDBC java.sql.Types-compatible codes, matching DuckDB JDBC where possible.
							MAP {
								'BOOLEAN': 16,
								'TINYINT': -6,
								'UTINYINT': 5,
								'SMALLINT': 5,
								'USMALLINT': 4,
								'INTEGER': 4,
								'UINTEGER': -5,
								'BIGINT': -5,
								'FLOAT': 6,
								'DOUBLE': 8,
								'DATE': 91,
								'TIME': 92,
								'TIME_NS': 92,
								'VARCHAR': 12,
								'BLOB': 2004,
								'DECIMAL': 3,
								'BIT': -7,
								'STRUCT': 2002,
							} AS type_codes
						FROM duckdb_columns()
						WHERE column_name LIKE %s
					) cols
					GROUP BY table_catalog, table_schema, table_name
				),
				constraints AS (
					SELECT
						database_name AS table_catalog,
						schema_name AS table_schema,
						table_name,
						LIST({
							constraint_name: constraint_name,
							constraint_type: constraint_type,
							constraint_column_names: constraint_column_names,
							constraint_column_usage: list_transform(
								referenced_column_names,
								lambda name: {
									fk_catalog: database_name,
									fk_db_schema: schema_name,
									fk_table: referenced_table,
									fk_column_name: name,
								}
							)
						}) table_constraints
					FROM duckdb_constraints()
					WHERE
						constraint_type NOT IN ('NOT NULL') AND
						list_has_any(
							constraint_column_names,
							list_filter(
								constraint_column_names,
								lambda name: name LIKE %s
							)
						)
					GROUP BY database_name, schema_name, table_name
				),
				tables AS (
					SELECT
						table_catalog catalog_name,
						table_schema schema_name,
						LIST({
							table_name: table_name,
							table_type: table_type,
							table_columns: COALESCE(table_columns, []),
							table_constraints: COALESCE(table_constraints, []),
						}) db_schema_tables
					FROM information_schema.tables
					LEFT JOIN columns
					USING (table_catalog, table_schema, table_name)
					LEFT JOIN constraints
					USING (table_catalog, table_schema, table_name)
					WHERE table_name LIKE %s%s
					GROUP BY table_catalog, table_schema
				),
				db_schemas AS (
					SELECT
						catalog_name,
						schema_name,
						COALESCE(db_schema_tables, []) AS db_schema_tables,
					FROM information_schema.schemata
					LEFT JOIN tables
					USING (catalog_name, schema_name)
					WHERE schema_name LIKE %s
				)

				SELECT
					catalog_name,
					COALESCE(LIST({
						db_schema_name: schema_name,
						db_schema_tables: db_schema_tables,
					}) FILTER (dbs.schema_name is not null), []) catalog_db_schemas
				FROM
					information_schema.schemata
				LEFT JOIN db_schemas dbs
				USING (catalog_name, schema_name)
				WHERE catalog_name LIKE %s
				GROUP BY catalog_name
				