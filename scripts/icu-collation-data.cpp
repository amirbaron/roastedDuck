// Exports the collation data of DuckDB's ICU 66 (extension/icu/third_party/icu, with the
// data DuckDB embeds in stubdata.cpp) for rustdb's port of ICU's collation runtime
// (crates/icu/src/icu4c/collation*.rs). Built and run by scripts/icu-collation-data.sh.
//
//   icu-collation-data export <out.bin>       write the data file
//   icu-collation-data keys <collation>       sort keys (hex) of the lines on stdin, as
//                                             DuckDB's icu_sort_key computes them
//
// The data file (little-endian):
//   "ICU66COL"                                    magic
//   u32 n, n bytes                                coll/ucadata.icu: the root collation data
//   u32 count; count × (u32 n, n bytes)           tailorings (RuleBasedCollator::cloneBinary)
//   u32 count; count × (u8 n, name, u32 index)    the bundles of the coll tree: the tailoring
//                                                 Collator::createInstance(Locale(name)) loads
//   u32 count; count × (u8 n, tag, u32 index)     the tags DuckDB opens with ucol_open
//   u32 count; count × (u32 start, u32 end, u16)  runs of non-zero FCD16 values (NFC data)
//   u32 count; count × (u32 c, u8 n, n × u32)     canonical decompositions (not Hangul)
//   u32 count; count × (u8 n, name)               Collator::getAvailableLocales, as DuckDB
//                                                 names the collations

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <map>
#include <memory>
#include <string>
#include <vector>

#include "unicode/coll.h"
#include "unicode/locid.h"
#include "unicode/normalizer2.h"
#include "unicode/tblcoll.h"
#include "unicode/ucol.h"
#include "unicode/udata.h"
#include "unicode/unistr.h"
#include "unicode/utf16.h"
#include "normalizer2impl.h"

extern "C" const unsigned char U_ICUDATA_ENTRY_POINT[];

namespace {

using std::string;
using std::vector;

[[noreturn]] void fail(const string &message) {
	std::cerr << "icu-collation-data: " << message << "\n";
	std::exit(1);
}

void check(UErrorCode status, const string &what) {
	if (U_FAILURE(status)) {
		fail(what + ": " + u_errorName(status));
	}
}

// The items of the common data package (`CmnD`, ucmndata.h): names and bytes.
struct PackageItem {
	string name;
	const uint8_t *data;
	uint32_t length;
};

uint32_t ReadU32(const uint8_t *p) {
	return uint32_t(p[0]) | uint32_t(p[1]) << 8 | uint32_t(p[2]) << 16 | uint32_t(p[3]) << 24;
}

vector<PackageItem> PackageItems() {
	const uint8_t *package = U_ICUDATA_ENTRY_POINT;
	uint16_t header_size = uint16_t(package[0] | package[1] << 8);
	const uint8_t *toc = package + header_size;
	uint32_t count = ReadU32(toc);
	vector<PackageItem> items;
	for (uint32_t i = 0; i < count; i++) {
		uint32_t name_offset = ReadU32(toc + 4 + 8 * i);
		uint32_t data_offset = ReadU32(toc + 8 + 8 * i);
		items.push_back({string(reinterpret_cast<const char *>(toc + name_offset)), toc + data_offset, 0});
	}
	// items are stored in TOC order; each one ends where the next begins
	for (uint32_t i = 0; i + 1 < count; i++) {
		items[i].length = uint32_t(items[i + 1].data - items[i].data);
	}
	return items;
}

struct Writer {
	vector<uint8_t> bytes;
	void U8(uint8_t value) {
		bytes.push_back(value);
	}
	void U16(uint16_t value) {
		U8(uint8_t(value));
		U8(uint8_t(value >> 8));
	}
	void U32(uint32_t value) {
		U16(uint16_t(value));
		U16(uint16_t(value >> 16));
	}
	void Bytes(const void *data, size_t length) {
		auto p = static_cast<const uint8_t *>(data);
		bytes.insert(bytes.end(), p, p + length);
	}
	void Name(const string &name) {
		if (name.size() > 255) {
			fail("name too long: " + name);
		}
		U8(uint8_t(name.size()));
		Bytes(name.data(), name.size());
	}
};

vector<uint8_t> CloneBinary(const icu::Collator &collator) {
	auto rbc = dynamic_cast<const icu::RuleBasedCollator *>(&collator);
	if (!rbc) {
		fail("not a RuleBasedCollator");
	}
	UErrorCode status = U_ZERO_ERROR;
	int32_t length = rbc->cloneBinary(nullptr, 0, status);
	if (status != U_BUFFER_OVERFLOW_ERROR) {
		check(status, "cloneBinary");
	}
	vector<uint8_t> result(length);
	status = U_ZERO_ERROR;
	rbc->cloneBinary(result.data(), length, status);
	check(status, "cloneBinary");
	return result;
}

// The tailorings, deduplicated.
struct Tailorings {
	vector<vector<uint8_t>> binaries;
	std::map<vector<uint8_t>, uint32_t> index;
	uint32_t Add(vector<uint8_t> binary) {
		auto entry = index.find(binary);
		if (entry != index.end()) {
			return entry->second;
		}
		uint32_t result = uint32_t(binaries.size());
		index.emplace(binary, result);
		binaries.push_back(std::move(binary));
		return result;
	}
};

// `IcuBindData(language, country)`.
std::unique_ptr<icu::Collator> CollatorFor(const string &language, const string &country) {
	UErrorCode status = U_ZERO_ERROR;
	icu::Locale locale(language.c_str(), country.c_str());
	if (locale.isBogus()) {
		fail("bogus locale " + language + "_" + country);
	}
	std::unique_ptr<icu::Collator> collator(icu::Collator::createInstance(locale, status));
	check(status, "createInstance " + language + "_" + country);
	return collator;
}

// `IcuBindData(tag)`.
std::unique_ptr<icu::Collator> CollatorForTag(const string &tag) {
	UErrorCode status = U_ZERO_ERROR;
	UCollator *ucollator = ucol_open(tag.c_str(), &status);
	check(status, "ucol_open " + tag);
	return std::unique_ptr<icu::Collator>(icu::Collator::fromUCollator(ucollator));
}

void Export(const char *path) {
	auto items = PackageItems();
	const string coll_prefix = string(U_ICUDATA_NAME) + "/coll/";
	Writer out;
	out.Bytes("ICU66COL", 8);

	// the root collation data
	const PackageItem *ucadata = nullptr;
	for (auto &item : items) {
		if (item.name == coll_prefix + "ucadata.icu") {
			ucadata = &item;
		}
	}
	if (!ucadata) {
		fail("no coll/ucadata.icu in the data package");
	}
	out.U32(ucadata->length);
	out.Bytes(ucadata->data, ucadata->length);

	// the bundles of the coll tree and the tailoring each one loads
	Tailorings tailorings;
	vector<std::pair<string, uint32_t>> bundles;
	for (auto &item : items) {
		const string &name = item.name;
		if (name.compare(0, coll_prefix.size(), coll_prefix) != 0 || name.size() < 4 ||
		    name.compare(name.size() - 4, 4, ".res") != 0) {
			continue;
		}
		string bundle = name.substr(coll_prefix.size(), name.size() - coll_prefix.size() - 4);
		if (bundle == "res_index" || bundle == "pool") {
			continue;
		}
		auto collator = CollatorFor(bundle, "");
		bundles.emplace_back(bundle, tailorings.Add(CloneBinary(*collator)));
	}
	// the tags of `LoadInternal`
	vector<std::pair<string, uint32_t>> tags;
	for (const string tag : {"und-u-ks-level1-kc-true"}) {
		auto collator = CollatorForTag(tag);
		tags.emplace_back(tag, tailorings.Add(CloneBinary(*collator)));
	}

	out.U32(uint32_t(tailorings.binaries.size()));
	for (auto &binary : tailorings.binaries) {
		out.U32(uint32_t(binary.size()));
		out.Bytes(binary.data(), binary.size());
	}
	out.U32(uint32_t(bundles.size()));
	for (auto &bundle : bundles) {
		out.Name(bundle.first);
		out.U32(bundle.second);
	}
	out.U32(uint32_t(tags.size()));
	for (auto &tag : tags) {
		out.Name(tag.first);
		out.U32(tag.second);
	}

	// FCD16 values and canonical decompositions of the NFC data the collation code uses
	UErrorCode status = U_ZERO_ERROR;
	const icu::Normalizer2Impl *nfc_impl = icu::Normalizer2Factory::getNFCImpl(status);
	check(status, "getNFCImpl");
	const icu::Normalizer2 *nfd = icu::Normalizer2::getNFDInstance(status);
	check(status, "getNFDInstance");
	struct Run {
		uint32_t start, end;
		uint16_t value;
	};
	vector<Run> runs;
	vector<std::pair<uint32_t, vector<uint32_t>>> decompositions;
	for (UChar32 c = 0; c <= 0x10FFFF; c++) {
		uint16_t fcd16 = nfc_impl->getFCD16(c);
		if (fcd16 != 0) {
			if (!runs.empty() && runs.back().end + 1 == uint32_t(c) && runs.back().value == fcd16) {
				runs.back().end = uint32_t(c);
			} else {
				runs.push_back({uint32_t(c), uint32_t(c), fcd16});
			}
		}
		if (c >= 0xAC00 && c <= 0xD7A3) {
			continue; // Hangul syllables decompose algorithmically
		}
		icu::UnicodeString decomposition;
		if (nfd->getDecomposition(c, decomposition)) {
			// getDecomposition is the full NFD mapping; check it against normalize()
			icu::UnicodeString normalized = nfd->normalize(icu::UnicodeString(c), status);
			check(status, "normalize");
			if (normalized != decomposition) {
				fail("decomposition differs from NFD");
			}
			vector<uint32_t> code_points;
			for (int32_t i = 0; i < decomposition.length();) {
				UChar32 d = decomposition.char32At(i);
				code_points.push_back(uint32_t(d));
				i += U16_LENGTH(d);
			}
			decompositions.emplace_back(uint32_t(c), code_points);
		}
	}
	out.U32(uint32_t(runs.size()));
	for (auto &run : runs) {
		out.U32(run.start);
		out.U32(run.end);
		out.U16(run.value);
	}
	out.U32(uint32_t(decompositions.size()));
	for (auto &decomposition : decompositions) {
		out.U32(decomposition.first);
		out.U8(uint8_t(decomposition.second.size()));
		for (auto d : decomposition.second) {
			out.U32(d);
		}
	}

	// the collations of `LoadInternal`
	int32_t count;
	const icu::Locale *locales = icu::Collator::getAvailableLocales(count);
	out.U32(uint32_t(count));
	for (int32_t i = 0; i < count; i++) {
		string name = locales[i].getLanguage();
		if (locales[i].getCountry()[0]) {
			name += "_";
			name += locales[i].getCountry();
		}
		out.Name(name);
	}

	FILE *file = std::fopen(path, "wb");
	if (!file || std::fwrite(out.bytes.data(), 1, out.bytes.size(), file) != out.bytes.size() ||
	    std::fclose(file) != 0) {
		fail(string("cannot write ") + path);
	}
	std::cerr << "icu-collation-data: " << bundles.size() << " bundles, " << tailorings.binaries.size()
	          << " tailorings, " << runs.size() << " FCD16 runs, " << decompositions.size()
	          << " decompositions, " << count << " collations, " << out.bytes.size() << " bytes\n";
}

// The sort key of every line on stdin under a DuckDB collation name (`ICUSortKeyBind`)
// or tag, in hex without the trailing zero byte (`ICUCollateFunction`).
void Keys(const string &collation) {
	std::unique_ptr<icu::Collator> collator;
	if (collation.find('-') != string::npos) {
		collator = CollatorForTag(collation);
	} else {
		auto split = collation.find('_');
		if (split == string::npos) {
			collator = CollatorFor(collation, "");
		} else {
			collator = CollatorFor(collation.substr(0, split), collation.substr(split + 1));
		}
	}
	string line;
	vector<uint8_t> buffer(64);
	while (std::getline(std::cin, line)) {
		icu::UnicodeString input = icu::UnicodeString::fromUTF8(icu::StringPiece(line.data(), int32_t(line.size())));
		int32_t size = collator->getSortKey(input, buffer.data(), int32_t(buffer.size()));
		if (size > int32_t(buffer.size())) {
			buffer.resize(size);
			size = collator->getSortKey(input, buffer.data(), int32_t(buffer.size()));
		}
		static const char HEX[] = "0123456789ABCDEF";
		string hex;
		for (int32_t i = 0; i + 1 < size; i++) {
			hex += HEX[buffer[i] / 16];
			hex += HEX[buffer[i] % 16];
		}
		std::cout << hex << "\n";
	}
}

} // namespace

int main(int argc, char **argv) {
	if (argc == 3 && string(argv[1]) == "export") {
		Export(argv[2]);
	} else if (argc == 3 && string(argv[1]) == "keys") {
		Keys(argv[2]);
	} else {
		fail("usage: icu-collation-data export <out.bin> | keys <collation>");
	}
	return 0;
}
