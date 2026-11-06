"""Frozen 1.5.6 module-helper signatures for argument diagnostics.

Derived from the pinned C++ wheel; upstream source commit
802a345bb627435d2a05d63cb74454fa98b60b5e. No engine state is initialized.
"""

SIGNATURES = {'aggregate': ("(df: pandas.DataFrame, aggr_expr: object, group_expr: str = '', *, connection: "
               'duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation',),
 'alias': ('(df: pandas.DataFrame, alias: str, *, connection: duckdb.DuckDBPyConnection = None) -> '
           '_duckdb.DuckDBPyRelation',),
 'append': ('(table_name: str, df: pandas.DataFrame, *, by_name: bool = False, connection: '
            'duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'arrow': ('(rows_per_batch: typing.SupportsInt | typing.SupportsIndex = 1000000, *, connection: '
           'duckdb.DuckDBPyConnection = None) -> pyarrow.lib.RecordBatchReader',
           '(arrow_object: object, *, connection: duckdb.DuckDBPyConnection = None) -> '
           '_duckdb.DuckDBPyRelation'),
 'begin': ('(*, connection: duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'checkpoint': ('(*, connection: duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'close': ('(*, connection: duckdb.DuckDBPyConnection = None) -> None',),
 'commit': ('(*, connection: duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'create_function': ('(name: str, function: collections.abc.Callable, parameters: object = None, '
                     'return_type: _duckdb._sqltypes.DuckDBPyType = None, *, type: '
                     '_duckdb._func.PythonUDFType = <PythonUDFType.NATIVE: 0>, null_handling: '
                     '_duckdb._func.FunctionNullHandling = <FunctionNullHandling.DEFAULT: 0>, '
                     'exception_handling: _duckdb.PythonExceptionHandling = '
                     '<PythonExceptionHandling.DEFAULT: 0>, side_effects: bool = False, connection: '
                     'duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'cursor': ('(*, connection: duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'default_connection': ('() -> duckdb.DuckDBPyConnection',),
 'description': ('(*, connection: duckdb.DuckDBPyConnection = None) -> typing.Optional[list]',),
 'df': ('(*, date_as_object: bool = False, connection: duckdb.DuckDBPyConnection = None) -> pandas.DataFrame',
        '(*, date_as_object: bool = False, connection: duckdb.DuckDBPyConnection = None) -> pandas.DataFrame',
        '(df: pandas.DataFrame, *, connection: duckdb.DuckDBPyConnection = None) -> '
        '_duckdb.DuckDBPyRelation'),
 'disable_profiling': ('(*, connection: duckdb.DuckDBPyConnection = None) -> None',),
 'distinct': ('(df: pandas.DataFrame, *, connection: duckdb.DuckDBPyConnection = None) -> '
              '_duckdb.DuckDBPyRelation',),
 'duplicate': ('(*, connection: duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'enable_profiling': ('(*, connection: duckdb.DuckDBPyConnection = None) -> None',),
 'execute': ('(query: object, parameters: object = None, *, connection: duckdb.DuckDBPyConnection = None) -> '
             'duckdb.DuckDBPyConnection',),
 'executemany': ('(query: object, parameters: object = None, *, connection: duckdb.DuckDBPyConnection = '
                 'None) -> duckdb.DuckDBPyConnection',),
 'extract_statements': ('(query: str, *, connection: duckdb.DuckDBPyConnection = None) -> list',),
 'fetch_arrow_table': ('(rows_per_batch: typing.SupportsInt | typing.SupportsIndex = 1000000, *, connection: '
                       'duckdb.DuckDBPyConnection = None) -> pyarrow.lib.Table',),
 'fetch_df': ('(*, date_as_object: bool = False, connection: duckdb.DuckDBPyConnection = None) -> '
              'pandas.DataFrame',),
 'fetch_df_chunk': ('(vectors_per_chunk: typing.SupportsInt | typing.SupportsIndex = 1, *, date_as_object: '
                    'bool = False, connection: duckdb.DuckDBPyConnection = None) -> pandas.DataFrame',),
 'fetch_record_batch': ('(rows_per_batch: typing.SupportsInt | typing.SupportsIndex = 1000000, *, '
                        'connection: duckdb.DuckDBPyConnection = None) -> pyarrow.lib.RecordBatchReader',),
 'fetchall': ('(*, connection: duckdb.DuckDBPyConnection = None) -> list',),
 'fetchdf': ('(*, date_as_object: bool = False, connection: duckdb.DuckDBPyConnection = None) -> '
             'pandas.DataFrame',),
 'fetchmany': ('(size: typing.SupportsInt | typing.SupportsIndex = 1, *, connection: '
               'duckdb.DuckDBPyConnection = None) -> list',),
 'fetchnumpy': ('(*, connection: duckdb.DuckDBPyConnection = None) -> dict',),
 'fetchone': ('(*, connection: duckdb.DuckDBPyConnection = None) -> typing.Optional[tuple]',),
 'filesystem_is_registered': ('(name: str, *, connection: duckdb.DuckDBPyConnection = None) -> bool',),
 'filter': ('(df: pandas.DataFrame, filter_expr: object, *, connection: duckdb.DuckDBPyConnection = None) -> '
            '_duckdb.DuckDBPyRelation',),
 'from_arrow': ('(arrow_object: object, *, connection: duckdb.DuckDBPyConnection = None) -> '
                '_duckdb.DuckDBPyRelation',),
 'from_csv_auto': ('(path_or_buffer: object, **kwargs) -> _duckdb.DuckDBPyRelation',),
 'from_df': ('(df: pandas.DataFrame, *, connection: duckdb.DuckDBPyConnection = None) -> '
             '_duckdb.DuckDBPyRelation',),
 'from_parquet': ('(path_or_buffer: object, binary_as_string: bool = False, *, file_row_number: bool = '
                  'False, filename: bool = False, hive_partitioning: bool = False, union_by_name: bool = '
                  'False, compression: object = None, connection: duckdb.DuckDBPyConnection = None) -> '
                  '_duckdb.DuckDBPyRelation',),
 'from_query': ("(query: object, *, alias: str = '', params: object = None, connection: "
                'duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation',),
 'get_profiling_information': ("(*, format: str = 'json', connection: duckdb.DuckDBPyConnection = None) -> "
                               'str',),
 'get_table_names': ('(query: str, *, qualified: bool = False, connection: duckdb.DuckDBPyConnection = None) '
                     '-> set[str]',),
 'install_extension': ('(extension: str, *, force_install: bool = False, repository: object = None, '
                       'repository_url: object = None, version: object = None, connection: '
                       'duckdb.DuckDBPyConnection = None) -> None',),
 'interrupt': ('(*, connection: duckdb.DuckDBPyConnection = None) -> None',),
 'limit': ('(df: pandas.DataFrame, n: typing.SupportsInt | typing.SupportsIndex, offset: typing.SupportsInt '
           '| typing.SupportsIndex = 0, *, connection: duckdb.DuckDBPyConnection = None) -> '
           '_duckdb.DuckDBPyRelation',),
 'list_filesystems': ('(*, connection: duckdb.DuckDBPyConnection = None) -> list',),
 'load_extension': ('(extension: str, *, connection: duckdb.DuckDBPyConnection = None) -> None',),
 'order': ('(df: pandas.DataFrame, order_expr: str, *, connection: duckdb.DuckDBPyConnection = None) -> '
           '_duckdb.DuckDBPyRelation',),
 'pl': ('(rows_per_batch: typing.SupportsInt | typing.SupportsIndex = 1000000, *, lazy: bool = False, '
        'connection: duckdb.DuckDBPyConnection = None) -> duckdb::PolarsDataFrame',),
 'project': ("(df: pandas.DataFrame, *args, groups: str = '', connection: duckdb.DuckDBPyConnection = None) "
             '-> _duckdb.DuckDBPyRelation',),
 'query': ("(query: object, *, alias: str = '', params: object = None, connection: duckdb.DuckDBPyConnection "
           '= None) -> _duckdb.DuckDBPyRelation',),
 'query_df': ('(df: pandas.DataFrame, virtual_table_name: str, sql_query: str, *, connection: '
              'duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation',),
 'query_progress': ('(*, connection: duckdb.DuckDBPyConnection = None) -> float',),
 'read_csv': ('(path_or_buffer: object, **kwargs) -> _duckdb.DuckDBPyRelation',),
 'read_json': ('(path_or_buffer: object, *, columns: typing.Optional[object] = None, sample_size: '
               'typing.Optional[object] = None, maximum_depth: typing.Optional[object] = None, records: '
               'typing.Optional[str] = None, format: typing.Optional[str] = None, date_format: '
               'typing.Optional[object] = None, timestamp_format: typing.Optional[object] = None, '
               'compression: typing.Optional[object] = None, maximum_object_size: typing.Optional[object] = '
               'None, ignore_errors: typing.Optional[object] = None, convert_strings_to_integers: '
               'typing.Optional[object] = None, field_appearance_threshold: typing.Optional[object] = None, '
               'map_inference_threshold: typing.Optional[object] = None, maximum_sample_files: '
               'typing.Optional[object] = None, filename: typing.Optional[object] = None, hive_partitioning: '
               'typing.Optional[object] = None, union_by_name: typing.Optional[object] = None, hive_types: '
               'typing.Optional[object] = None, hive_types_autocast: typing.Optional[object] = None, '
               'connection: duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation',),
 'read_parquet': ('(path_or_buffer: object, binary_as_string: bool = False, *, file_row_number: bool = '
                  'False, filename: bool = False, hive_partitioning: bool = False, union_by_name: bool = '
                  'False, compression: object = None, connection: duckdb.DuckDBPyConnection = None) -> '
                  '_duckdb.DuckDBPyRelation',),
 'register': ('(view_name: str, python_object: object, *, connection: duckdb.DuckDBPyConnection = None) -> '
              'duckdb.DuckDBPyConnection',),
 'register_filesystem': ('(filesystem: fsspec.AbstractFileSystem, *, connection: duckdb.DuckDBPyConnection = '
                         'None) -> None',),
 'remove_function': ('(name: str, *, connection: duckdb.DuckDBPyConnection = None) -> '
                     'duckdb.DuckDBPyConnection',),
 'rollback': ('(*, connection: duckdb.DuckDBPyConnection = None) -> duckdb.DuckDBPyConnection',),
 'rowcount': ('(*, connection: duckdb.DuckDBPyConnection = None) -> int',),
 'set_default_connection': ('(connection: duckdb.DuckDBPyConnection) -> None',),
 'sql': ("(query: object, *, alias: str = '', params: object = None, connection: duckdb.DuckDBPyConnection = "
         'None) -> _duckdb.DuckDBPyRelation',),
 'table': ('(table_name: str, *, connection: duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation',),
 'table_function': ('(name: str, parameters: object = None, *, connection: duckdb.DuckDBPyConnection = None) '
                    '-> _duckdb.DuckDBPyRelation',),
 'to_arrow_reader': ('(batch_size: typing.SupportsInt | typing.SupportsIndex = 1000000, *, connection: '
                     'duckdb.DuckDBPyConnection = None) -> pyarrow.lib.RecordBatchReader',),
 'to_arrow_table': ('(batch_size: typing.SupportsInt | typing.SupportsIndex = 1000000, *, connection: '
                    'duckdb.DuckDBPyConnection = None) -> pyarrow.lib.Table',),
 'unregister': ('(view_name: str, *, connection: duckdb.DuckDBPyConnection = None) -> '
                'duckdb.DuckDBPyConnection',),
 'unregister_filesystem': ('(name: str, *, connection: duckdb.DuckDBPyConnection = None) -> None',),
 'values': ('(*args, connection: duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation',),
 'view': ('(view_name: str, *, connection: duckdb.DuckDBPyConnection = None) -> _duckdb.DuckDBPyRelation',),
 'write_csv': ('(df: pandas.DataFrame, filename: str, *, sep: object = None, na_rep: object = None, header: '
               'object = None, quotechar: object = None, escapechar: object = None, date_format: object = '
               'None, timestamp_format: object = None, quoting: object = None, encoding: object = None, '
               'compression: object = None, overwrite: object = None, per_thread_output: object = None, '
               'use_tmp_file: object = None, partition_by: object = None, write_partition_columns: object = '
               'None, connection: duckdb.DuckDBPyConnection = None) -> None',)}
