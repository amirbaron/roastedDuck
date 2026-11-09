// Catch runner and test-directory helpers for DuckDB's C API tests built against roastedduck's
// libduckdb (DuckDB's test/unittest.cpp and test/helpers/test_helpers.cpp, reduced to what
// the C API tests use). Test-only; never shipped.
#define CATCH_CONFIG_RUNNER
#include "catch.hpp"
#include "test_helpers.hpp"

#include <cstdlib>
#include <dirent.h>
#include <sys/stat.h>
#include <unistd.h>
#include <duckdb.h>
#include <dlfcn.h>
#include <filesystem>
#if defined(__APPLE__)
#include <mach-o/dyld.h>
#elif defined(__linux__)
#include <link.h>
#endif

// Optional release-test guard. It checks the resolved symbol and every loaded
// DuckDB image inside the actual test process, before and after Catch runs.
static bool SelectedEngineImage() {
	auto requested = std::getenv("DUCKDB_INSTALL_LIB");
	if (!requested) return true;
	Dl_info symbol{};
	if (!dladdr(reinterpret_cast<void *>(duckdb_library_version), &symbol) || !symbol.dli_fname ||
	    !std::filesystem::equivalent(symbol.dli_fname, requested)) return false;
	struct Images { const char *expected; size_t count = 0; bool valid = true; } images{requested};
	auto inspect = [](Images &state, const char *name) {
		if (!name || !*name) return;
		auto filename = std::filesystem::path(name).filename();
		if (filename == "libduckdb.so" || filename == "libduckdb.dylib" || filename == "Cduckdb") {
			state.count++;
			state.valid = state.valid && std::filesystem::equivalent(name, state.expected);
		}
	};
#if defined(__APPLE__)
	for (uint32_t i = 0; i < _dyld_image_count(); i++) inspect(images, _dyld_get_image_name(i));
#elif defined(__linux__)
	struct Callback { Images *images; decltype(inspect) *inspect_image; } callback{&images, &inspect};
	dl_iterate_phdr([](dl_phdr_info *info, size_t, void *opaque) {
		auto state = static_cast<Callback *>(opaque);
		(*state->inspect_image)(*state->images, info->dlpi_name);
		return 0;
	}, &callback);
#endif
	if (!images.valid || images.count != 1) return false;
	fprintf(stderr, "Verified selected DuckDB image: %s\n", std::filesystem::canonical(requested).c_str());
	return true;
}

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
		test_directory = string(base ? base : "/tmp/roastedduck-capi-tests") + "/" + to_string(getpid());
		mkdir(base ? base : "/tmp/roastedduck-capi-tests", 0755);
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
	if (!SelectedEngineImage()) return 102;
	auto result = Catch::Session().run(argc, argv);
	if (!duckdb::test_directory.empty()) {
		duckdb::RemoveTree(duckdb::test_directory);
	}
	return SelectedEngineImage() ? result : 103;
}
