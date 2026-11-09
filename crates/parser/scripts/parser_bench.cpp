// Benchmark of DuckDB's Parser::ParseQuery (grammar + transformer) for comparison with
// roastedduck-parser's `parse_bench` example.
//
// Build (from the workspace root, REF = the DuckDB 1.5.6 checkout with a release build):
//   c++ -std=c++17 -O2 -I$REF/src/include crates/parser/scripts/parser_bench.cpp \
//       -L$REF/build/release/src -lduckdb -Wl,-rpath,$REF/build/release/src -o target/parser_bench
// Run:
//   target/parser_bench <corpus> <iterations>
// The corpus is a sequence of records `<byte length>\n<sql>` (crates/pg-parser/scripts).
#include "duckdb/parser/parser.hpp"

#include <cstdio>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

static std::vector<std::string> ReadCorpus(const char *path) {
	std::ifstream in(path, std::ios::binary);
	std::stringstream buffer;
	buffer << in.rdbuf();
	std::string data = buffer.str();
	std::vector<std::string> result;
	size_t pos = 0;
	while (pos < data.size()) {
		size_t newline = data.find('\n', pos);
		if (newline == std::string::npos) {
			break;
		}
		size_t length = std::stoul(data.substr(pos, newline - pos));
		result.push_back(data.substr(newline + 1, length));
		pos = newline + 1 + length;
	}
	return result;
}

int main(int argc, char **argv) {
	if (argc < 3) {
		fprintf(stderr, "usage: %s <corpus> <iterations>\n", argv[0]);
		return 1;
	}
	auto corpus = ReadCorpus(argv[1]);
	int iterations = std::atoi(argv[2]);
	size_t statements = 0;
	size_t errors = 0;
	for (int iteration = 0; iteration < iterations; iteration++) {
		for (auto &sql : corpus) {
			try {
				duckdb::Parser parser;
				parser.ParseQuery(sql);
				statements += parser.statements.size();
			} catch (std::exception &) {
				errors++;
			}
		}
	}
	printf("%zu queries, %zu statements, %zu errors\n", corpus.size() * iterations, statements, errors);
	return 0;
}
