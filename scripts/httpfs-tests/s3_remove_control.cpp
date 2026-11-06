// Separate authenticated C++ SDK oracle; never linked into the Rust engine.
#include "duckdb.hpp"
#include "duckdb/common/file_system.hpp"
#include "duckdb/main/client_context_file_opener.hpp"
#include "duckdb/parser/parsed_data/create_scalar_function_info.hpp"
#include "duckdb/execution/expression_executor_state.hpp"
#include <fstream>
#include <iomanip>
#include <iostream>
#include <sstream>

using namespace duckdb;

static std::string Quote(const std::string &value) {
    std::ostringstream out;
    out << '"';
    for (unsigned char c : value) {
        if (c == '"') out << "\\\"";
        else if (c == '\\') out << "\\\\";
        else if (c < 32) out << "\\u" << std::hex << std::setw(4) << std::setfill('0') << unsigned(c) << std::dec;
        else out << c;
    }
    out << '"';
    return out.str();
}

int main(int argc, char **argv) {
    if (argc != 4) return 2;
    DBConfig config;
    for (const auto *name : {"allow_unsigned_extensions", "allow_community_extensions", "autoinstall_known_extensions", "autoload_known_extensions"}) config.SetOption(name, Value(false));
    DuckDB db(nullptr, &config);
    Connection con(db);
    ScalarFunction function("probe_remove", {LogicalType::VARCHAR}, LogicalType::BOOLEAN,
        [](DataChunk &input, ExpressionState &state, Vector &output) {
            auto &context = state.GetContext();
            auto &fs = FileSystem::GetFileSystem(context);
            for (idx_t row = 0; row < input.size(); row++) {
                fs.RemoveFile(input.GetValue(0, row).ToString());
                output.SetValue(row, Value::BOOLEAN(true));
            }
        });
    function.SetStability(FunctionStability::VOLATILE);
    CreateScalarFunctionInfo info(function);
    con.context->RegisterFunction(info);
    struct Step { std::string label; std::string sql; bool success; };
    std::string endpoint = argv[1], package = argv[2];
    std::string escaped;
    for (char c : package) { escaped += c; if (c == '\'') escaped += '\''; }
    std::vector<Step> steps = {
        {"load", "LOAD '" + escaped + "'", true},
        {"secret", "CREATE SECRET credentials (TYPE s3, KEY_ID 'fixture-access', SECRET 'fixture-secret', REGION 'fixture-region', ENDPOINT '" + endpoint + "', URL_STYLE 'path', USE_SSL false)", true},
        {"part-size", "SET s3_uploader_max_filesize='50MB'", true},
        {"create", "COPY (SELECT 1 i) TO 's3://bucket/delete-single.csv' (FORMAT CSV)", true},
        {"remove", "SELECT probe_remove('s3://bucket/delete-single.csv')", true},
        {"missing", "SELECT probe_remove('s3://bucket/delete-single.csv')", false},
        {"create-denied", "COPY (SELECT 2 i) TO 's3://bucket/deny-single.csv' (FORMAT CSV)", true},
        {"denied", "SELECT probe_remove('s3://bucket/deny-single.csv')", false},
        {"disable", "SET disabled_filesystems='S3FileSystem'", true},
        {"disabled", "SELECT probe_remove('s3://bucket/deny-single.csv')", false},
        {"database-valid", "SELECT 42", true}
    };
    std::ofstream out(argv[3]);
    out << '[';
    bool first = true;
    for (const auto &step : steps) {
        auto result = con.Query(step.sql);
        if (result->HasError() == step.success) {
            std::cerr << "Unexpected SDK outcome at " << step.label << ": "
                      << (result->HasError() ? result->GetError() : "success") << std::endl;
            return 3;
        }
        if (!first) out << ',';
        first = false;
        out << "{\"label\":" << Quote(step.label) << ",\"sql\":" << Quote(step.sql)
            << ",\"status\":" << (result->HasError() ? 1 : 0) << ",\"error\":";
        if (result->HasError()) out << Quote(result->GetError()); else out << "null";
        out << ",\"rows\":[";
        if (!result->HasError()) {
            for (idx_t row = 0; row < result->RowCount(); row++) {
                if (row) out << ',';
                out << '[';
                for (idx_t col = 0; col < result->ColumnCount(); col++) {
                    if (col) out << ',';
                    out << Quote(result->GetValue(col, row).ToString());
                }
                out << ']';
            }
        }
        out << "]}";
    }
    out << "]\n";
    return out.good() ? 0 : 4;
}
