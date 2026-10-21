// Catch runner and test-directory helpers for DuckDB's C API tests built against rustdb's
// libduckdb (DuckDB's test/unittest.cpp and test/helpers/test_helpers.cpp, reduced to what
// the C API tests use). Test-only; never shipped.
#define CATCH_CONFIG_RUNNER
#include "catch.hpp"
#include "test_helpers.hpp"

#include <cstdlib>
#include <dirent.h>
#include <sys/stat.h>
#include <unistd.h>

namespace duckdb {

static string test_directory;

static void RemoveTree(const string &path) {
	auto dir = opendir(path.c_str());
	if (!dir) {
		unlink(path.c_str());
		return;
	}
	while (auto entry = readdir(dir)) {
		string name = entry->d_name;
		if (name == "." || name == "..") {
			continue;
		}
		RemoveTree(path + "/" + name);
	}
	closedir(dir);
	rmdir(path.c_str());
}

string TestDirectoryPath() {
	if (test_directory.empty()) {
		auto base = getenv("CAPI_TEST_DIR");
		test_directory = string(base ? base : "/tmp/rustdb-capi-tests") + "/" + to_string(getpid());
		mkdir(base ? base : "/tmp/rustdb-capi-tests", 0755);
		mkdir(test_directory.c_str(), 0755);
	}
	return test_directory;
}

string TestCreatePath(string suffix) {
	return TestDirectoryPath() + "/" + suffix;
}

void TestDeleteFile(string path) {
	unlink(path.c_str());
}

void DeleteDatabase(string path) {
	unlink(path.c_str());
	unlink((path + ".wal").c_str());
}

} // namespace duckdb

int main(int argc, char *argv[]) {
	auto result = Catch::Session().run(argc, argv);
	if (!duckdb::test_directory.empty()) {
		duckdb::RemoveTree(duckdb::test_directory);
	}
	return result;
}
