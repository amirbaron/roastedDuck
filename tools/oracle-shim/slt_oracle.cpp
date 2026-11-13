// C++ side of the oracle shim: thin wrappers that reproduce what DuckDB's own sqllogictest runner
// (test/sqlite/*.cpp, test/helpers/test_helpers.cpp) does with engine internals.

#include "slt_oracle.h"

#include "duckdb.hpp"
#include "duckdb/catalog/catalog_search_path.hpp"
#include "duckdb/common/file_system.hpp"
#include "duckdb/common/serializer/buffered_file_writer.hpp"
#include "duckdb/common/sql_identifier.hpp"
#include "duckdb/main/client_context.hpp"
#include "duckdb/main/client_data.hpp"
#include "duckdb/main/extension_helper.hpp"
#include "duckdb/main/settings.hpp"
#include "duckdb/storage/storage_info.hpp"

#include <cstdlib>
#include <cstring>
#include <thread>

using namespace duckdb;

struct slt_config {
	unique_ptr<DBConfig> config;
};

struct slt_db {
	unique_ptr<DuckDB> db;
};

struct slt_conn {
	unique_ptr<Connection> con;
};

struct slt_result {
	unique_ptr<MaterializedQueryResult> result;
	bool has_error = false;
	string error;
	vector<string> names;
	vector<string> type_names;
	vector<LogicalType> types;
	idx_t column_count = 0;
	idx_t row_count = 0;
	vector<string> values;
};

struct slt_session {
	ClientConfig client_config;
	vector<CatalogSearchEntry> search_paths;
	string query_log_path;
};

namespace {

string MakeString(const char *data, size_t len) {
	return string(data, len);
}

void SetError(char **error, const string &message) {
	if (!error) {
		return;
	}
	auto buffer = static_cast<char *>(std::malloc(message.size() + 1));
	if (!buffer) {
		return;
	}
	std::memcpy(buffer, message.c_str(), message.size() + 1);
	*error = buffer;
}

void SetError(char **error, std::exception &ex) {
	ErrorData data(ex);
	SetError(error, data.Message());
}

slt_result *ErrorResult(const string &message) {
	auto result = new slt_result();
	result->has_error = true;
	result->error = message;
	return result;
}

// TestResultHelper::SQLLogicTestConvertValue
string ConvertValue(ClientContext &context, const Value &value, const LogicalType &sql_type,
                    bool original_sqlite_test) {
	if (value.IsNull()) {
		return "NULL";
	}
	if (original_sqlite_test) {
		switch (sql_type.id()) {
		case LogicalTypeId::DECIMAL:
		case LogicalTypeId::FLOAT:
		case LogicalTypeId::DOUBLE:
			return value.CastAs(context, LogicalType::BIGINT).ToString();
		default:
			break;
		}
	}
	if (sql_type.id() == LogicalTypeId::BOOLEAN) {
		return BooleanValue::Get(value) ? "1" : "0";
	}
	auto str = value.CastAs(context, LogicalType::VARCHAR).ToString();
	if (str.empty()) {
		return "(empty)";
	}
	return StringUtil::Replace(str, string("\0", 1), "\\0");
}

// The parts of GetTestConfig() (test/helpers/test_helpers.cpp) for the default test configuration,
// plus the non-setting option the SQLLogicTestRunner constructor changes.
unique_ptr<DBConfig> TestConfig() {
	auto result = make_uniq<DBConfig>();
	result->options.checkpoint_wal_size = 0;
	result->options.checkpoint_on_shutdown = true;
	result->options.abort_on_wal_failure = true;
	result->SetOptionByName("allow_unsigned_extensions", true);
	Storage::VerifyBlockAllocSize(DEFAULT_BLOCK_ALLOC_SIZE);
	result->SetOptionByName("default_block_size", Value::UBIGINT(DEFAULT_BLOCK_ALLOC_SIZE));
	result->options.debug_initialize = DebugInitialize::NO_INITIALIZE;
	result->SetOptionByName("debug_verify_vector", "NONE");
	result->options.load_extensions = false;
	return result;
}

} // namespace

extern "C" {

int32_t slt_abi_version(void) {
	return SLT_ORACLE_ABI_VERSION;
}

const char *slt_build_directory(void) {
	return SLT_DUCKDB_BUILD_DIRECTORY;
}

void slt_string_free(char *str) {
	std::free(str);
}

slt_config *slt_config_new(char **error) {
	try {
		auto config = new slt_config();
		config->config = TestConfig();
		return config;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return nullptr;
	}
}

void slt_config_free(slt_config *config) {
	delete config;
}

bool slt_config_set_option(slt_config *config, const char *name, size_t name_len, const char *value,
                           size_t value_len, char **error) {
	try {
		config->config->SetOptionByName(MakeString(name, name_len), Value(MakeString(value, value_len)));
		return true;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return false;
	}
}

void slt_config_set_read_only(slt_config *config, bool read_only) {
	auto &options = config->config->options;
	options.use_temporary_directory = !read_only;
	options.access_mode = read_only ? AccessMode::READ_ONLY : AccessMode::AUTOMATIC;
}

bool slt_config_set_storage_version(slt_config *config, const char *version, size_t version_len, char **error) {
	try {
		config->config->options.serialization_compatibility =
		    SerializationCompatibility::FromString(MakeString(version, version_len));
		return true;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return false;
	}
}

void slt_config_inherit_storage_version(slt_config *config, const slt_db *db) {
	config->config->options.serialization_compatibility =
	    db->db->instance->config.options.serialization_compatibility;
}

uint64_t slt_config_setting(const slt_config *config, int32_t setting) {
	auto &db_config = *config->config;
	switch (setting) {
	case SLT_SETTING_DEFAULT_BLOCK_SIZE:
		return Settings::Get<DefaultBlockSizeSetting>(db_config);
	case SLT_SETTING_AUTOLOAD_KNOWN_EXTENSIONS:
		return Settings::Get<AutoloadKnownExtensionsSetting>(db_config) ? 1 : 0;
	case SLT_SETTING_ALLOW_UNSIGNED_EXTENSIONS:
		return Settings::Get<AllowUnsignedExtensionsSetting>(db_config) ? 1 : 0;
	case SLT_SETTING_VACUUM_REBUILD_INDEXES:
		return Settings::Get<VacuumRebuildIndexesSetting>(db_config);
	default:
		return 0;
	}
}

slt_db *slt_db_open(const char *path, size_t path_len, const slt_config *config, char **error) {
	try {
		auto db = new slt_db();
		try {
			db->db = make_uniq<DuckDB>(MakeString(path, path_len), config->config.get());
		} catch (...) {
			delete db;
			throw;
		}
		return db;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return nullptr;
	}
}

void slt_db_free(slt_db *db) {
	delete db;
}

int32_t slt_db_load_extension(slt_db *db, const char *name, size_t name_len, char **error) {
	try {
		auto extension = MakeString(name, name_len);
		switch (ExtensionHelper::LoadExtension(*db->db, extension)) {
		case ExtensionLoadResult::LOADED_EXTENSION:
			return SLT_EXTENSION_LOADED;
		case ExtensionLoadResult::EXTENSION_UNKNOWN:
			return SLT_EXTENSION_UNKNOWN;
		default:
			break;
		}
		// Reference tooling only: official release libraries need not statically link
		// FTS/VSS. An explicit directory lets unchanged "require" statements load the
		// pinned oracle artifacts using ordinary signature and version validation.
		// Other extensions and callers without this opt-in retain their old verdict.
		auto directory_env = std::getenv("SLT_ORACLE_EXTENSION_DIRECTORY");
		if (!directory_env || (extension != "fts" && extension != "vss")) {
			return SLT_EXTENSION_NOT_LOADED;
		}
		string directory(directory_env);
		auto &fs = FileSystem::GetFileSystem(*db->db->instance);
		if (directory.empty() || !fs.IsPathAbsolute(directory) || !fs.DirectoryExists(directory)) {
			throw InvalidInputException("SLT_ORACLE_EXTENSION_DIRECTORY must name an existing absolute directory");
		}
		auto path = fs.JoinPath(directory, extension + ".duckdb_extension");
		string quoted;
		for (auto c : path) {
			quoted += c;
			if (c == '\'') {
				quoted += '\'';
			}
		}
		Connection connection(*db->db);
		auto result = connection.Query("LOAD '" + quoted + "'");
		if (result->HasError()) {
			SetError(error, result->GetError());
			return SLT_EXTENSION_ERROR;
		}
		return SLT_EXTENSION_LOADED;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return SLT_EXTENSION_ERROR;
	}
}

slt_conn *slt_conn_new(slt_db *db, char **error) {
	try {
		auto conn = new slt_conn();
		conn->con = make_uniq<Connection>(*db->db);
		return conn;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return nullptr;
	}
}

void slt_conn_free(slt_conn *conn) {
	delete conn;
}

void slt_conn_set_test_client_options(slt_conn *conn) {
	auto &client_config = ClientConfig::GetConfig(*conn->con->context);
	client_config.enable_progress_bar = true;
	client_config.print_progress_bar = false;
}

slt_session *slt_session_save(slt_conn *main_conn, slt_config *config) {
	auto &context = *main_conn->con->context;
	config->config->options = context.db->config.options;
	config->config->user_settings = context.db->config.user_settings;
	auto session = new slt_session();
	session->client_config = context.config;
	session->search_paths = context.client_data->catalog_search_path->GetSetPaths();
	if (context.client_data->log_query_writer) {
		session->query_log_path = context.client_data->log_query_writer->path;
	}
	return session;
}

bool slt_session_restore(slt_conn *main_conn, const slt_session *session, char **error) {
	try {
		auto &con = *main_conn->con;
		con.context->config = session->client_config;
		con.BeginTransaction();
		con.context->client_data->catalog_search_path->Set(session->search_paths, CatalogSetPathType::SET_SCHEMAS);
		con.Commit();
		if (!session->query_log_path.empty()) {
			con.context->client_data->log_query_writer = make_uniq<BufferedFileWriter>(
			    FileSystem::GetFileSystem(*con.context), session->query_log_path, 1 << 1 | 1 << 5);
		}
		return true;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return false;
	}
}

void slt_session_free(slt_session *session) {
	delete session;
}

slt_result *slt_query(slt_conn *conn, const char *sql, size_t sql_len) {
	unique_ptr<MaterializedQueryResult> materialized;
	try {
		QueryParameters parameters;
		parameters.output_type = QueryResultOutputType::FORCE_MATERIALIZED;
		parameters.memory_type = QueryResultMemoryType::BUFFER_MANAGED;
		auto result = conn->con->context->Query(MakeString(sql, sql_len), parameters);
		materialized = unique_ptr_cast<QueryResult, MaterializedQueryResult>(std::move(result));
	} catch (std::exception &ex) {
		materialized = make_uniq<MaterializedQueryResult>(ErrorData(ex));
	}
	auto result = new slt_result();
	if (materialized->HasError()) {
		result->has_error = true;
		result->error = materialized->GetError();
	} else {
		result->names = materialized->names;
		result->types = materialized->types;
		for (auto &type : result->types) {
			result->type_names.push_back(type.ToString());
		}
		result->column_count = materialized->ColumnCount();
		result->row_count = materialized->RowCount();
	}
	result->result = std::move(materialized);
	return result;
}

void slt_result_free(slt_result *result) {
	delete result;
}

const char *slt_result_error(const slt_result *result) {
	return result->has_error ? result->error.c_str() : nullptr;
}

uint64_t slt_result_column_count(const slt_result *result) {
	return result->column_count;
}

uint64_t slt_result_row_count(const slt_result *result) {
	return result->row_count;
}

const char *slt_result_column_name(const slt_result *result, uint64_t column) {
	return column < result->names.size() ? result->names[column].c_str() : "";
}

const char *slt_result_column_type(const slt_result *result, uint64_t column) {
	return column < result->type_names.size() ? result->type_names[column].c_str() : "";
}

int32_t slt_result_column_class(const slt_result *result, uint64_t column) {
	if (column >= result->types.size()) {
		return SLT_COLUMN_OTHER;
	}
	auto &type = result->types[column];
	if (type.IsNumeric()) {
		return SLT_COLUMN_NUMERIC;
	}
	if (type == LogicalType::BOOLEAN) {
		return SLT_COLUMN_BOOLEAN;
	}
	return SLT_COLUMN_OTHER;
}

bool slt_result_render(slt_result *result, slt_conn *context, bool original_sqlite_test, char **error) {
	if (!result->result || result->has_error) {
		return true;
	}
	try {
		auto &materialized = *result->result;
		auto &client = *context->con->context;
		vector<string> values;
		values.reserve(result->row_count * result->column_count);
		for (idx_t r = 0; r < result->row_count; r++) {
			for (idx_t c = 0; c < result->column_count; c++) {
				values.push_back(ConvertValue(client, materialized.GetValue(c, r), result->types[c], original_sqlite_test));
			}
		}
		result->values = std::move(values);
		return true;
	} catch (std::exception &ex) {
		SetError(error, ex);
		return false;
	}
}

uint64_t slt_result_value_count(const slt_result *result) {
	return result->values.size();
}

const char *slt_result_value(const slt_result *result, uint64_t index, size_t *len) {
	auto &value = result->values[index];
	*len = value.size();
	return value.c_str();
}

bool slt_values_equal(slt_conn *context, const slt_result *result, uint64_t column, const char *actual,
                      size_t actual_len, const char *expected, size_t expected_len) {
	try {
		auto &client = *context->con->context;
		auto &sql_type = result->types[column];
		auto convert = [&](const string &str, Value &value) {
			if (str == "NULL") {
				value = Value(sql_type);
				return true;
			}
			value = Value(str);
			return value.TryCastAs(client, sql_type);
		};
		Value lvalue, rvalue;
		if (!convert(MakeString(actual, actual_len), lvalue) || !convert(MakeString(expected, expected_len), rvalue)) {
			return false;
		}
		return Value::ValuesAreEqual(client, lvalue, rvalue);
	} catch (std::exception &) {
		return false;
	}
}

slt_result *slt_load_result_file(slt_conn *context, const char *path, size_t path_len, const char *const *names,
                                 const size_t *name_lens, uint64_t name_count) {
	auto fname = MakeString(path, path_len);
	try {
		DuckDB db(nullptr);
		Connection con(db);
		auto threads = MaxValue<idx_t>(std::thread::hardware_concurrency(), 1);
		con.Query("PRAGMA threads=" + to_string(threads));

		string struct_definition = "STRUCT_PACK(";
		for (idx_t i = 0; i < name_count; i++) {
			if (i > 0) {
				struct_definition += ", ";
			}
			struct_definition += StringUtil::Format("%s := VARCHAR", SQLIdentifier(MakeString(names[i], name_lens[i])));
		}
		struct_definition += ")";

		auto csv_result = con.Query("SELECT * FROM read_csv('" + fname +
		                            "', header=1, sep='|', columns=" + struct_definition + ", auto_detect=false)");
		if (csv_result->HasError()) {
			return ErrorResult(StringUtil::Format("Could not read CSV File \"%s\": %s", fname, csv_result->GetError()));
		}
		auto result = new slt_result();
		result->column_count = csv_result->ColumnCount();
		auto &client = *context->con->context;
		while (true) {
			auto chunk = csv_result->Fetch();
			if (!chunk || chunk->size() == 0) {
				break;
			}
			for (idx_t r = 0; r < chunk->size(); r++) {
				for (idx_t c = 0; c < chunk->ColumnCount(); c++) {
					result->values.push_back(chunk->GetValue(c, r).CastAs(client, LogicalType::VARCHAR).ToString());
				}
			}
		}
		return result;
	} catch (std::exception &ex) {
		ErrorData data(ex);
		return ErrorResult(data.Message());
	}
}

} // extern "C"
