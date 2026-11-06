"""DuckDB Python API backed by the native Rust port.

The full completion inventory lives in docs/python-api-inventory.json in the source tree.
"""
import sys as _sys
import builtins as _builtins
from . import _duckdb as _native
_sys.modules["_duckdb"] = _native
from ._duckdb import *  # noqa: F403
from ._duckdb import __version__, __duckdb_version__, __standard_vector_size__, __formatted_python_version__, __interactive__, __jupyter__
from . import sqltypes
from ._version import version

_default = None

def _is_connection(value):
    # Native handle casters inspect the actual type, not a spoofed __class__.
    return issubclass(_builtins.type(value), DuckDBPyConnection)

def default_connection():
    global _default
    if _default is None or _default._closed:
        _default = _native.connect(":memory:")
    return _default

def set_default_connection(connection):
    global _default
    if not _is_connection(connection):
        _native._set_default_connection_error((connection,), None)
    _default = connection

def _clean_default_connection():
    global _default
    if _default is not None:
        _default.close()
    _default = None

def connect(*args, **kwargs): return _native.connect(*args, **kwargs)

def execute(query, parameters=None, *, connection=None):
    return (connection if connection is not None else default_connection()).execute(query, parameters)

def executemany(query, parameters=None, *, connection=None):
    return (connection if connection is not None else default_connection()).executemany(query, parameters)

def fetchone(*, connection=None): return (connection if connection is not None else default_connection()).fetchone()
def fetchall(*, connection=None): return (connection if connection is not None else default_connection()).fetchall()
def fetchmany(size=1, *, connection=None): return (connection if connection is not None else default_connection()).fetchmany(size)
def description(*, connection=None): return (connection if connection is not None else default_connection()).description
def rowcount(*, connection=None): return (connection if connection is not None else default_connection()).rowcount
def cursor(*, connection=None): return (connection if connection is not None else default_connection()).cursor()
def duplicate(*, connection=None): return (connection if connection is not None else default_connection()).duplicate()
def close(*, connection=None): return (connection if connection is not None else default_connection()).close()
def begin(*, connection=None): return (connection if connection is not None else default_connection()).begin()
def commit(*, connection=None): return (connection if connection is not None else default_connection()).commit()
def rollback(*, connection=None): return (connection if connection is not None else default_connection()).rollback()
def checkpoint(*, connection=None): return (connection if connection is not None else default_connection()).checkpoint()
def interrupt(*, connection=None): return (connection if connection is not None else default_connection()).interrupt()
def query_progress(*, connection=None): return (connection if connection is not None else default_connection()).query_progress()

type = _native.type
dtype = _native.dtype
row_type = _native.row_type

from ._dbapi_type_object import BINARY, DATETIME, NUMBER, ROWID, STRING, DBAPITypeObject
from .value.constant import *

def sql(query, *, alias="", params=None, connection=None):
    return (connection if connection is not None else default_connection()).sql(query, alias=alias, params=params)
def query(query, *, alias="", params=None, connection=None):
    return (connection if connection is not None else default_connection()).query(query, alias=alias, params=params)
def from_query(query, *, alias="", params=None, connection=None):
    return (connection if connection is not None else default_connection()).from_query(query, alias=alias, params=params)
def table(table_name, *, connection=None): return (connection if connection is not None else default_connection()).table(table_name)
def view(view_name, *, connection=None): return (connection if connection is not None else default_connection()).view(view_name)
def from_arrow(arrow_object, *, connection=None): return (connection if connection is not None else default_connection()).from_arrow(arrow_object)
def from_df(df, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df)
def query_df(df, virtual_table_name, sql_query, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df).query(virtual_table_name, sql_query)
def project(df, expressions, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df).project(expressions)
def alias(df, alias, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df).set_alias(alias)
def filter(df, filter_expr, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df).filter(filter_expr)
def order(df, order_expr, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df).order(order_expr)
def distinct(df, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df).distinct()
def append(*args, **kwargs): return _native._append(*args, **kwargs)
def register(view_name, python_object, *, connection=None): return (connection if connection is not None else default_connection()).register(view_name, python_object)
def unregister(view_name, *, connection=None): return (connection if connection is not None else default_connection()).unregister(view_name)

def fetchnumpy(*, connection=None): return (connection if connection is not None else default_connection()).fetchnumpy()
def fetchdf(*, date_as_object=False, connection=None): return (connection if connection is not None else default_connection()).fetchdf(date_as_object=date_as_object)
fetch_df = fetchdf
def _dataframe_call(name, args, kwargs):
    def bad():
        return _native._dataframe_arguments_error(name, args, kwargs)
    fields = {"df": ("df",), "aggregate": ("df", "aggr_expr", "group_expr"), "limit": ("df", "n", "offset")}[name]
    if len(args) > len(fields) or any(key not in (*fields, "connection", "date_as_object") for key in kwargs):
        return bad()
    if any(key in kwargs for key in fields[:len(args)]):
        return bad()
    values = dict(zip(fields, args))
    values.update(kwargs)
    con = values.get("connection")
    if con is not None and not _is_connection(con):
        return bad()
    if name == "df" and "df" not in values:
        if not isinstance(values.get("date_as_object", False), bool):
            return bad()
        return (con if con is not None else default_connection()).fetchdf(date_as_object=values.get("date_as_object", False))
    if "date_as_object" in values:
        return bad()
    pandas = _sys.modules.get("pandas")
    if pandas is None or not isinstance(values.get("df"), pandas.DataFrame):
        return bad()
    if name == "aggregate":
        if "aggr_expr" not in values or not isinstance(values.get("group_expr", ""), str):
            return bad()
    if name == "limit":
        import operator
        try:
            values["n"] = operator.index(values["n"])
            values["offset"] = operator.index(values.get("offset", 0))
            if any(value < -(1 << 63) or value >= (1 << 63) for value in (values["n"], values["offset"])):
                return bad()
        except (KeyError, TypeError, OverflowError):
            return bad()
    rel = (con if con is not None else default_connection()).from_df(values["df"])
    if name == "aggregate":
        return rel.aggregate(values["aggr_expr"], values.get("group_expr", ""))
    if name == "limit":
        return rel.limit(values["n"], values["offset"])
    return rel

def df(*args, **kwargs): return _dataframe_call("df", args, kwargs)
def aggregate(*args, **kwargs): return _dataframe_call("aggregate", args, kwargs)
def limit(*args, **kwargs): return _dataframe_call("limit", args, kwargs)
def to_arrow_table(rows_per_batch=1000000, *, connection=None): return (connection if connection is not None else default_connection()).to_arrow_table(rows_per_batch)
def fetch_arrow_table(rows_per_batch=1000000, *, connection=None): return (connection if connection is not None else default_connection()).fetch_arrow_table(rows_per_batch)
def to_arrow_reader(batch_size=1000000, *, connection=None): return (connection if connection is not None else default_connection()).to_arrow_reader(batch_size)
def arrow(rows_per_batch=1000000, *, connection=None):
    con = connection if connection is not None else default_connection()
    if isinstance(rows_per_batch, int) or hasattr(rows_per_batch, "__index__"):
        return con.arrow(rows_per_batch)
    return con.from_arrow(rows_per_batch)
def fetch_record_batch(rows_per_batch=1000000, *, connection=None): return (connection if connection is not None else default_connection()).fetch_record_batch(rows_per_batch)

def fetch_df_chunk(vectors_per_chunk=1, *, date_as_object=False, connection=None): return (connection if connection is not None else default_connection()).fetch_df_chunk(vectors_per_chunk, date_as_object=date_as_object)

from enum import IntEnum as _IntEnum
import types as _types
class _DuckDBEnum(_IntEnum):
    def __str__(self): return f"{_builtins.type(self).__name__}.{self.name}"
class PythonExceptionHandling(_DuckDBEnum):
    DEFAULT = 0
    RETURN_NULL = 1
class PythonUDFType(_DuckDBEnum):
    NATIVE = 0
    ARROW = 1
class FunctionNullHandling(_DuckDBEnum):
    DEFAULT = 0
    SPECIAL = 1
PythonExceptionHandling.__module__ = "_duckdb"
PythonUDFType.__module__ = FunctionNullHandling.__module__ = "_duckdb._func"
DEFAULT, RETURN_NULL = PythonExceptionHandling.DEFAULT, PythonExceptionHandling.RETURN_NULL
_native.PythonExceptionHandling = PythonExceptionHandling
_native.DEFAULT, _native.RETURN_NULL = DEFAULT, RETURN_NULL
_func = _types.ModuleType("_duckdb._func")
_func.PythonUDFType, _func.FunctionNullHandling = PythonUDFType, FunctionNullHandling
_func.NATIVE, _func.ARROW = PythonUDFType.NATIVE, PythonUDFType.ARROW
_func.DEFAULT, _func.SPECIAL = FunctionNullHandling.DEFAULT, FunctionNullHandling.SPECIAL
_native._func = _func
_sys.modules["_duckdb._func"] = _func

def create_function(name, function, parameters=None, return_type=None, *, type=PythonUDFType.NATIVE, null_handling=FunctionNullHandling.DEFAULT, exception_handling=PythonExceptionHandling.DEFAULT, side_effects=False, connection=None):
    return (connection if connection is not None else default_connection()).create_function(name,function,parameters,return_type,type=type,null_handling=null_handling,exception_handling=exception_handling,side_effects=side_effects)
def remove_function(name, *, connection=None): return (connection if connection is not None else default_connection()).remove_function(name)

def table_function(name, parameters=None, *, connection=None): return (connection if connection is not None else default_connection()).table_function(name,parameters)

class RenderMode(_DuckDBEnum):
    ROWS = 0
    COLUMNS = 1
RenderMode.__module__ = "_duckdb"
ROWS, COLUMNS = RenderMode.ROWS, RenderMode.COLUMNS
_native.RenderMode = RenderMode
_native.ROWS, _native.COLUMNS = ROWS, COLUMNS

_native.create_function = create_function
_native.remove_function = remove_function
_native.table_function = table_function

def values(*args,connection=None): return (connection if connection is not None else default_connection()).values(*args)
_native.values = values

def describe(df, *, connection=None): return (connection if connection is not None else default_connection()).from_df(df).describe()
_native.describe = describe

class CSVLineTerminator(_DuckDBEnum):
    LINE_FEED = 0
    CARRIAGE_RETURN_LINE_FEED = 1
CSVLineTerminator.__module__ = "_duckdb"
_native.CSVLineTerminator = CSVLineTerminator
LINE_FEED, CARRIAGE_RETURN_LINE_FEED = CSVLineTerminator.LINE_FEED, CSVLineTerminator.CARRIAGE_RETURN_LINE_FEED
_native.LINE_FEED, _native.CARRIAGE_RETURN_LINE_FEED = LINE_FEED, CARRIAGE_RETURN_LINE_FEED

def read_csv(path_or_buffer, *, connection=None, **kwargs): return (connection if connection is not None else default_connection()).read_csv(path_or_buffer,**kwargs)
def from_csv_auto(path_or_buffer, *, connection=None, **kwargs): return (connection if connection is not None else default_connection()).from_csv_auto(path_or_buffer,**kwargs)
def write_csv(df, file_name, *, connection=None, **kwargs): return (connection if connection is not None else default_connection()).from_df(df).write_csv(file_name,**kwargs)
_native.read_csv, _native.from_csv_auto, _native.write_csv = read_csv, from_csv_auto, write_csv

def read_parquet(path_or_buffer, binary_as_string=False, *, file_row_number=False, filename=False, hive_partitioning=False, union_by_name=False, compression=None, connection=None):
    return (connection if connection is not None else default_connection()).read_parquet(path_or_buffer,binary_as_string,file_row_number=file_row_number,filename=filename,hive_partitioning=hive_partitioning,union_by_name=union_by_name,compression=compression)
from_parquet = read_parquet
_native.read_parquet, _native.from_parquet = read_parquet, from_parquet


def get_profiling_information(*, connection=None, format="json"):
    return (connection if connection is not None else default_connection()).get_profiling_information(format)
def enable_profiling(*, connection=None): return (connection if connection is not None else default_connection()).enable_profiling()
def disable_profiling(*, connection=None): return (connection if connection is not None else default_connection()).disable_profiling()
_native.get_profiling_information = get_profiling_information
_native.enable_profiling = enable_profiling
_native.disable_profiling = disable_profiling

def read_json(*args, connection=None, **kwargs): return (connection if connection is not None else default_connection()).read_json(*args, **kwargs)
_native.read_json = read_json

def pl(rows_per_batch=1000000, *, lazy=False, connection=None):
    return (connection if connection is not None else default_connection()).pl(rows_per_batch, lazy=lazy)
_native.pl = pl


def extract_statements(query, *, connection=None): return (connection if connection is not None else default_connection()).extract_statements(query)
def get_table_names(query, *, qualified=False, connection=None): return (connection if connection is not None else default_connection()).get_table_names(query, qualified=qualified)
def install_extension(extension, *, force_install=False, repository=None, repository_url=None, version=None, connection=None):
    return (connection if connection is not None else default_connection()).install_extension(extension, force_install=force_install, repository=repository, repository_url=repository_url, version=version)
def load_extension(extension, *, connection=None): return (connection if connection is not None else default_connection()).load_extension(extension)
_native.extract_statements = extract_statements
_native.get_table_names = get_table_names
_native.install_extension = install_extension
_native.load_extension = load_extension
for _statement_enum in (StatementType, ExpectedResultType, token_type):
    for _statement_name, _statement_value in _statement_enum.__members__.items():
        setattr(_native, _statement_name, _statement_value)


def register_filesystem(filesystem, *, connection=None): return (connection if connection is not None else default_connection()).register_filesystem(filesystem)
def unregister_filesystem(name, *, connection=None): return (connection if connection is not None else default_connection()).unregister_filesystem(name)
def list_filesystems(*, connection=None): return (connection if connection is not None else default_connection()).list_filesystems()
def filesystem_is_registered(name, *, connection=None): return (connection if connection is not None else default_connection()).filesystem_is_registered(name)
_native.register_filesystem = register_filesystem
_native.unregister_filesystem = unregister_filesystem
_native.list_filesystems = list_filesystems
_native.filesystem_is_registered = filesystem_is_registered



# The frozen low-level module exposes the same default-connection helpers.
# These remain thin frontend calls into the native Rust connection methods.
for _helper_name in ('aggregate', 'alias', 'append', 'arrow', 'to_arrow_reader', 'to_arrow_table', 'begin', 'checkpoint', 'close', 'commit', 'cursor', 'default_connection', 'description', 'df', 'distinct', 'dtype', 'duplicate', 'execute', 'executemany', 'fetch_arrow_table', 'fetch_df', 'fetch_df_chunk', 'fetch_record_batch', 'fetchall', 'fetchdf', 'fetchmany', 'fetchnumpy', 'fetchone', 'filter', 'from_arrow', 'from_df', 'from_query', 'interrupt', 'limit', 'order', 'project', 'query', 'query_df', 'query_progress', 'register', 'rollback', 'row_type', 'rowcount', 'set_default_connection', 'sql', 'table', 'type', 'unregister', 'view'):
    setattr(_native, _helper_name, globals()[_helper_name])


def _argument_repr(value):
    try:
        text = repr(value)
        text.encode("utf-8")
        return text
    except BaseException:
        # Match the frozen binder's diagnostic when user repr code fails.
        return "<repr raised Error>"


def _module_argument_error(name, args, kwargs):
    from ._module_signatures import SIGNATURES
    signatures = "\n".join(f"    {index}. {signature}" for index, signature in enumerate(SIGNATURES[name], 1))
    invoked = ", ".join(_argument_repr(value) for value in args)
    if kwargs:
        invoked += ("; kwargs: " if args else "kwargs: ") + ", ".join(
            f"{key}={_argument_repr(value)}" for key, value in kwargs.items()
        )
    raise TypeError(
        f"{name}(): incompatible function arguments. The following argument types are supported:\n"
        f"{signatures}\n\nInvoked with: {invoked}"
    )


def _checked_module_helper(name, function):
    from functools import wraps

    @wraps(function)
    def checked(*args, **kwargs):
        connection = kwargs.get("connection")
        if (connection is not None or name == "set_default_connection" and "connection" in kwargs) and not _is_connection(connection):
            _module_argument_error(name, args, kwargs)
        if name in ("sql", "query", "from_query"):
            alias = kwargs.get("alias")
            supplied = "alias" in kwargs
            if supplied and not _native._string_argument_valid(alias):
                _module_argument_error(name, args, kwargs)
        if name in ("table", "view", "register", "unregister", "table_function", "create_function", "remove_function"):
            field = "name" if name in ("table_function", "create_function", "remove_function") else "table_name" if name == "table" else "view_name"
            supplied = bool(args) or field in kwargs
            value = args[0] if args else kwargs.get(field)
            if supplied and not _native._string_argument_valid(value):
                _module_argument_error(name, args, kwargs)
        try:
            return function(*args, **kwargs)
        except TypeError as error:
            # A Python call-binding failure never entered the helper body. Errors
            # from native execution or user callbacks retain their original trace.
            if error.__traceback__.tb_next is not None:
                raise
        # Raise after leaving the handler so the replaced Python binding error is
        # not retained as __context__ on the public exception.
        _module_argument_error(name, args, kwargs)

    checked.__name__ = name
    return checked


from ._module_signatures import SIGNATURES as _module_signatures
for _helper_name in _module_signatures:
    _helper = _checked_module_helper(_helper_name, globals()[_helper_name])
    globals()[_helper_name] = _helper
    setattr(_native, _helper_name, _helper)
