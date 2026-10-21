// Reference zstd encoder for rustdb-zstd's byte comparisons: the zstd 1.5.6 that DuckDB
// vendors (linked from libduckdb), driven the way DuckDB drives it. Dev tooling only; it is
// never part of a cargo build.
//
// Usage:
//   zstd_ref oneshot <level> <input> <output>
//       ZSTD_compress (Parquet pages, temporary files).
//   zstd_ref strings <input> <output> <first_space> <page_space>
//       DuckDB's ZSTD string compression (zstd.cpp CompressString): <input> holds one
//       vector's strings, each a little-endian u32 length and its bytes; every string is
//       one ZSTD_compressStream2 call (ZSTD_e_continue, ZSTD_e_end for the last; empty
//       strings other than the last are skipped) at the default level into an output
//       buffer of <first_space> bytes, then of <page_space> bytes whenever the call
//       leaves output to flush (a new page).
//   zstd_ref stream <level> <input> <output> <chunk>
//       ZstdStreamWrapper (CSV/JSON writers): ZSTD_e_continue per <chunk> input bytes,
//       then ZSTD_e_end; 0 as level keeps the default.
//   zstd_ref bench <level> <input> <iterations>
//       Compression (ZSTD_compress) and decompression (ZSTD_decompress) throughput.
#include "zstd.h"

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iterator>
#include <string>
#include <vector>

using namespace duckdb_zstd;

static std::vector<char> ReadFile(const char *path) {
	std::ifstream in(path, std::ios::binary);
	return std::vector<char>(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

static void WriteFile(const char *path, const std::vector<char> &data) {
	std::ofstream out(path, std::ios::binary);
	out.write(data.data(), data.size());
}

static void Check(size_t result) {
	if (ZSTD_isError(result)) {
		fprintf(stderr, "zstd error: %s\n", ZSTD_getErrorName(result));
		exit(1);
	}
}

static int OneShot(int level, const char *in, const char *out) {
	auto src = ReadFile(in);
	std::vector<char> dst(ZSTD_compressBound(src.size()));
	auto size = ZSTD_compress(dst.data(), dst.size(), src.data(), src.size(), level);
	Check(size);
	dst.resize(size);
	WriteFile(out, dst);
	return 0;
}

static int Strings(const char *in, const char *out, size_t first_space, size_t page_space) {
	auto src = ReadFile(in);
	std::vector<std::pair<const char *, size_t>> strings;
	for (size_t pos = 0; pos + 4 <= src.size();) {
		uint32_t length;
		memcpy(&length, src.data() + pos, 4);
		strings.emplace_back(src.data() + pos + 4, length);
		pos += 4 + length;
	}
	auto context = ZSTD_createCCtx();
	ZSTD_CCtx_reset(context, ZSTD_reset_session_only);
	ZSTD_CCtx_refCDict(context, nullptr);
	ZSTD_CCtx_setParameter(context, ZSTD_c_compressionLevel, ZSTD_defaultCLevel());
	std::vector<char> result;
	std::vector<char> page(first_space);
	ZSTD_outBuffer out_buffer = {page.data(), page.size(), 0};
	for (size_t i = 0; i < strings.size(); i++) {
		bool end = i + 1 == strings.size();
		if (!end && strings[i].second == 0) {
			continue;
		}
		ZSTD_inBuffer in_buffer = {strings[i].first, strings[i].second, 0};
		while (true) {
			auto remaining = ZSTD_compressStream2(context, &out_buffer, &in_buffer, end ? ZSTD_e_end : ZSTD_e_continue);
			Check(remaining);
			if (remaining == 0) {
				break;
			}
			result.insert(result.end(), page.data(), page.data() + out_buffer.pos);
			page.assign(page_space, 0);
			out_buffer = {page.data(), page.size(), 0};
		}
	}
	result.insert(result.end(), page.data(), page.data() + out_buffer.pos);
	ZSTD_freeCCtx(context);
	WriteFile(out, result);
	return 0;
}

static int Stream(int level, const char *in, const char *out, size_t chunk) {
	auto src = ReadFile(in);
	auto context = ZSTD_createCStream();
	if (level != 0) {
		ZSTD_CCtx_setParameter(context, ZSTD_c_compressionLevel, level);
	}
	std::vector<char> result;
	std::vector<char> buffer(ZSTD_CStreamOutSize());
	for (size_t pos = 0; pos < src.size(); pos += chunk) {
		ZSTD_inBuffer in_buffer = {src.data() + pos, std::min(chunk, src.size() - pos), 0};
		while (in_buffer.pos < in_buffer.size) {
			ZSTD_outBuffer out_buffer = {buffer.data(), buffer.size(), 0};
			Check(ZSTD_compressStream2(context, &out_buffer, &in_buffer, ZSTD_e_continue));
			result.insert(result.end(), buffer.data(), buffer.data() + out_buffer.pos);
		}
	}
	while (true) {
		ZSTD_inBuffer in_buffer = {nullptr, 0, 0};
		ZSTD_outBuffer out_buffer = {buffer.data(), buffer.size(), 0};
		auto remaining = ZSTD_compressStream2(context, &out_buffer, &in_buffer, ZSTD_e_end);
		Check(remaining);
		result.insert(result.end(), buffer.data(), buffer.data() + out_buffer.pos);
		if (remaining == 0) {
			break;
		}
	}
	ZSTD_freeCStream(context);
	WriteFile(out, result);
	return 0;
}

static int Bench(int level, const char *in, int iterations) {
	auto src = ReadFile(in);
	std::vector<char> dst(ZSTD_compressBound(src.size()));
	std::vector<char> back(src.size());
	size_t size = 0;
	auto start = std::chrono::steady_clock::now();
	for (int i = 0; i < iterations; i++) {
		size = ZSTD_compress(dst.data(), dst.size(), src.data(), src.size(), level);
		Check(size);
	}
	auto middle = std::chrono::steady_clock::now();
	for (int i = 0; i < iterations; i++) {
		Check(ZSTD_decompress(back.data(), back.size(), dst.data(), size));
	}
	auto end = std::chrono::steady_clock::now();
	double mb = double(src.size()) * iterations / (1 << 20);
	printf("compressed %zu -> %zu: compress %.1f MB/s, decompress %.1f MB/s\n", src.size(), size,
	       mb / std::chrono::duration<double>(middle - start).count(),
	       mb / std::chrono::duration<double>(end - middle).count());
	return 0;
}

int main(int argc, char **argv) {
	if (argc >= 5 && std::string(argv[1]) == "bench") {
		return Bench(atoi(argv[2]), argv[3], atoi(argv[4]));
	}
	if (argc >= 5 && std::string(argv[1]) == "oneshot") {
		return OneShot(atoi(argv[2]), argv[3], argv[4]);
	}
	if (argc >= 6 && std::string(argv[1]) == "strings") {
		return Strings(argv[2], argv[3], strtoull(argv[4], nullptr, 10), strtoull(argv[5], nullptr, 10));
	}
	if (argc >= 6 && std::string(argv[1]) == "stream") {
		return Stream(atoi(argv[2]), argv[3], argv[4], strtoull(argv[5], nullptr, 10));
	}
	fprintf(stderr, "usage: zstd_ref oneshot <level> <in> <out> | strings <in> <out> <first_space> <page_space> | "
	                "stream <level> <in> <out> <chunk>\n");
	return 2;
}
