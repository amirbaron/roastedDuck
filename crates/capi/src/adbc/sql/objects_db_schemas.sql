
				WITH db_schemas AS (
					SELECT
						catalog_name,
						schema_name,
					FROM information_schema.schemata
					WHERE schema_name LIKE %s
				)

				SELECT
					catalog_name,
					COALESCE(LIST({
						db_schema_name: schema_name,
						db_schema_tables: []::STRUCT(
							table_name VARCHAR,
							table_type VARCHAR,
							table_columns STRUCT(
								column_name VARCHAR,
								ordinal_position INTEGER,
								remarks VARCHAR,
								xdbc_data_type SMALLINT,
								xdbc_type_name VARCHAR,
								xdbc_column_size INTEGER,
								xdbc_decimal_digits SMALLINT,
								xdbc_num_prec_radix SMALLINT,
								xdbc_nullable SMALLINT,
								xdbc_column_def VARCHAR,
								xdbc_sql_data_type SMALLINT,
								xdbc_datetime_sub SMALLINT,
								xdbc_char_octet_length INTEGER,
								xdbc_is_nullable VARCHAR,
								xdbc_scope_catalog VARCHAR,
								xdbc_scope_schema VARCHAR,
								xdbc_scope_table VARCHAR,
								xdbc_is_autoincrement BOOLEAN,
								xdbc_is_generatedcolumn BOOLEAN
							)[],
							table_constraints STRUCT(
								constraint_name VARCHAR,
								constraint_type VARCHAR,
								constraint_column_names VARCHAR[],
								constraint_column_usage STRUCT(fk_catalog VARCHAR, fk_db_schema VARCHAR, fk_table VARCHAR, fk_column_name VARCHAR)[]
							)[]
						)[],
					}) FILTER (dbs.schema_name is not null), []) catalog_db_schemas
				FROM
					information_schema.schemata
				LEFT JOIN db_schemas dbs
				USING (catalog_name, schema_name)
				WHERE catalog_name LIKE %s
				GROUP BY catalog_name
				