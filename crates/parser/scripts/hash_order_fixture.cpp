// Generates src/hash_order_fixture.txt (c++ -std=c++17 hash_order_fixture.cpp && ./a.out >
// ../src/hash_order_fixture.txt): iteration orders of libc++ unordered_map with DuckDB's CIHash, for the
// parser's hash_order tests. Each case: keys inserted with operator[] (unique), then
// the order after insertion, after a copy-assignment into a fresh map, and after a
// second copy-assignment into that map.
#include <cstdint>
#include <cstdio>
#include <random>
#include <string>
#include <unordered_map>
#include <vector>

static uint64_t CIHash(const std::string &str) {
	uint32_t hash = 0;
	for (unsigned char c : str) {
		hash += static_cast<uint32_t>(tolower(c));
		hash += hash << 10;
		hash ^= hash >> 6;
	}
	hash += hash << 3;
	hash ^= hash >> 11;
	hash += hash << 15;
	return hash;
}

struct Hash {
	uint64_t operator()(const std::string &s) const {
		return CIHash(s);
	}
};
struct Eq {
	bool operator()(const std::string &a, const std::string &b) const {
		if (a.size() != b.size()) {
			return false;
		}
		for (size_t i = 0; i < a.size(); i++) {
			if (tolower((unsigned char)a[i]) != tolower((unsigned char)b[i])) {
				return false;
			}
		}
		return true;
	}
};
using Map = std::unordered_map<std::string, int, Hash, Eq>;

static void print(const char *label, const Map &map) {
	printf("%s", label);
	for (auto &entry : map) {
		printf(" %s", entry.first.c_str());
	}
	printf("\n");
}

int main() {
	std::mt19937 rng(42);
	const char *alphabet = "abcdefghijklmnopqrstuvwxyz0123456789_";
	for (int c = 0; c < 300; c++) {
		int n = 1 + rng() % 40;
		Map map;
		printf("case");
		std::vector<std::string> keys;
		for (int i = 0; i < n; i++) {
			std::string key;
			int len = 1 + rng() % 6;
			for (int j = 0; j < len; j++) {
				key += alphabet[rng() % 37];
			}
			keys.push_back(key);
			printf(" %s", key.c_str());
		}
		printf("\n");
		for (auto &key : keys) {
			map[key] = 1;
		}
		print("unique", map);
		Map copy;
		copy = map;
		print("copy", copy);
		copy = map;
		print("copy2", copy);
		map.clear();
		map["zz"] = 1;
		map["a1"] = 1;
		print("cleared", map);
	}
	return 0;
}
