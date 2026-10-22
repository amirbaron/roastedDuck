// Exports the collation data of DuckDB's ICU 66 (extension/icu/third_party/icu, with the
// data DuckDB embeds in stubdata.cpp) for rustdb's port of ICU's collation runtime
// (crates/icu/src/icu4c/collation*.rs). Built and run by scripts/icu-collation-data.sh.
//
//   icu-collation-data types                  the collation types of the coll tree
//   icu-collation-data export <out.bin> <types...>
//                                             write the data file
//   icu-collation-data keys <collation>       sort keys (hex) of the lines on stdin, as
//                                             DuckDB's icu_sort_key computes them
//
// The data file (little-endian):
//   "ICU66COL"                                    magic
//   u32 n, n bytes                                coll/ucadata.icu: the root collation data
//   u32 count; count × (u32 n, n bytes)           tailorings (RuleBasedCollator::cloneBinary)
//   u32 count; count × (u8 n, type)               the collation types of the coll tree
//   u32 count; count × (u8 n, name, u32 index,    the bundles of the coll tree: the tailoring
//     u32 types, types × (u8 n, type, u32 index)) Collator::createInstance(Locale(name)) loads,
//                                                 and Locale(name@collation=type) where it differs
//                                                 (index 0xffffffff: U_MISSING_RESOURCE_ERROR)
//   u32 count; count × (u8 n, tag, u32 index)     the tags DuckDB opens with ucol_open
//   u32 count; count × (u32 start, u32 end, u16)  runs of non-zero FCD16 values (NFC data)
//   u32 count; count × (u32 c, u8 n, n × u32)     canonical decompositions (not Hangul)
//   256 bytes                                     smallFCD: per 32 code units, may some have
//                                                 FCD16 != 0 (lead surrogates: their code points)
//   u32 count; count × (u8 n, from, u8 n, to)     uloc_getName's ISO 639-2 to 639-1 language codes
//   u32 count; count × (u8 n, from, u8 n, to)     uloc_getName's ISO 3166 alpha-3 to alpha-2 codes
//   u32 count; count × (u8 n, name, u32 code)     the names of the scripts
//   u32 count; count × (u8 n, legacy key,         keyTypeData: the locale keywords' keys and
//     u8 n, BCP 47 key, u8 special types,         types (uloc_toLegacyKey, uloc_toLegacyType):
//     u32 types, types × (u8 n, name,             every name of a type (legacy, BCP 47, alias)
//     u8 n, legacy type))                         with its legacy type
//   u32 count; count × (u8 n, name)               Collator::getAvailableLocales, as DuckDB
//                                                 names the collations

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <map>
#include <memory>
#include <algorithm>
#include <set>
#include <string>
#include <vector>

#include "unicode/coll.h"
#include "unicode/locid.h"
#include "unicode/normalizer2.h"
#include "unicode/tblcoll.h"
#include "unicode/ucol.h"
#include "unicode/uchar.h"
#include "unicode/udata.h"
#include "unicode/uloc.h"
#include "unicode/unistr.h"
#include "unicode/ures.h"
#include "unicode/utf16.h"
#include "normalizer2impl.h"
#include "ucol_imp.h"
#include "uresimp.h"

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

// The tailoring index of a collation whose creation fails with U_MISSING_RESOURCE_ERROR.
const uint32_t MISSING_RESOURCE = 0xffffffff;

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

// The collation types of the coll tree (the keys of the collations tables), one per line.
// This opens the bundles with ures_openDirect, whose cache entries lack their parents and
// so change what later collators load: it runs in a process of its own.
void Types() {
	auto items = PackageItems();
	const string coll_prefix = string(U_ICUDATA_NAME) + "/coll/";
	std::set<string> types;
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
		UErrorCode status = U_ZERO_ERROR;
		UResourceBundle *resource = ures_openDirect(U_ICUDATA_COLL, bundle.c_str(), &status);
		check(status, "ures_openDirect " + bundle);
		UResourceBundle *collations = ures_getByKey(resource, "collations", nullptr, &status);
		if (U_SUCCESS(status)) {
			for (int32_t i = 0; i < ures_getSize(collations); i++) {
				UResourceBundle *type = ures_getByIndex(collations, i, nullptr, &status);
				check(status, "collation type of " + bundle);
				if (string(ures_getKey(type)) != "default") {
					types.insert(ures_getKey(type));
				}
				ures_close(type);
			}
		}
		ures_close(collations);
		ures_close(resource);
	}
	for (auto &type : types) {
		std::cout << type << "\n";
	}
}

// The UTF-16 string of a resource as an invariant-character string.
string ResourceString(UResourceBundle *resource) {
	UErrorCode status = U_ZERO_ERROR;
	int32_t length;
	const UChar *chars = ures_getString(resource, &length, &status);
	check(status, "ures_getString");
	string result;
	for (int32_t i = 0; i < length; i++) {
		result += char(chars[i]);
	}
	return result;
}

// keyTypeData (uloc_keytype.cpp's initFromResourceBundle): for every key, its legacy and
// BCP 47 ids, its special types (CODEPOINTS 1, REORDER_CODE 2, RG_KEY_VALUE 4), and every
// name of every type (legacy id, BCP 47 id, aliases) with the type's legacy id.
void KeyTypeData(Writer &out) {
	UErrorCode status = U_ZERO_ERROR;
	UResourceBundle *data = ures_openDirect(nullptr, "keyTypeData", &status);
	UResourceBundle *key_map = ures_getByKey(data, "keyMap", nullptr, &status);
	UResourceBundle *type_map = ures_getByKey(data, "typeMap", nullptr, &status);
	check(status, "keyTypeData");
	UErrorCode alias_status = U_ZERO_ERROR;
	UResourceBundle *type_alias = ures_getByKey(data, "typeAlias", nullptr, &alias_status);
	UErrorCode bcp_alias_status = U_ZERO_ERROR;
	UResourceBundle *bcp_type_alias = ures_getByKey(data, "bcpTypeAlias", nullptr, &bcp_alias_status);
	struct Key {
		string legacy, bcp;
		uint8_t special = 0;
		vector<std::pair<string, string>> types;
	};
	vector<Key> keys;
	for (int32_t k = 0; k < ures_getSize(key_map); k++) {
		UResourceBundle *entry = ures_getByIndex(key_map, k, nullptr, &status);
		check(status, "keyMap");
		Key key;
		key.legacy = ures_getKey(entry);
		key.bcp = ResourceString(entry);
		if (key.bcp.empty()) {
			key.bcp = key.legacy;
		}
		ures_close(entry);
		bool tz = key.legacy == "timezone";
		auto slashes = [&](string id) {
			if (tz) {
				std::replace(id.begin(), id.end(), ':', '/');
			}
			return id;
		};
		UErrorCode by_key_status = U_ZERO_ERROR;
		UResourceBundle *aliases = U_SUCCESS(alias_status)
		                               ? ures_getByKey(type_alias, key.legacy.c_str(), nullptr, &by_key_status)
		                               : nullptr;
		if (U_FAILURE(by_key_status)) {
			aliases = nullptr;
		}
		by_key_status = U_ZERO_ERROR;
		UResourceBundle *bcp_aliases = U_SUCCESS(bcp_alias_status)
		                                   ? ures_getByKey(bcp_type_alias, key.bcp.c_str(), nullptr, &by_key_status)
		                                   : nullptr;
		if (U_FAILURE(by_key_status)) {
			bcp_aliases = nullptr;
		}
		UResourceBundle *types = ures_getByKey(type_map, key.legacy.c_str(), nullptr, &status);
		check(status, "typeMap " + key.legacy);
		for (int32_t t = 0; t < ures_getSize(types); t++) {
			UResourceBundle *type = ures_getByIndex(types, t, nullptr, &status);
			check(status, "typeMap entry");
			string legacy = ures_getKey(type);
			if (legacy == "CODEPOINTS" || legacy == "REORDER_CODE" || legacy == "RG_KEY_VALUE") {
				key.special |= legacy == "CODEPOINTS" ? 1 : legacy == "REORDER_CODE" ? 2 : 4;
				ures_close(type);
				continue;
			}
			legacy = slashes(legacy);
			string bcp = ResourceString(type);
			if (bcp.empty()) {
				bcp = legacy;
			}
			ures_close(type);
			key.types.emplace_back(legacy, legacy);
			if (bcp != legacy) {
				key.types.emplace_back(bcp, legacy);
			}
			for (auto *alias_table : {aliases, bcp_aliases}) {
				if (!alias_table) {
					continue;
				}
				const string &target = alias_table == aliases ? legacy : bcp;
				for (int32_t a = 0; a < ures_getSize(alias_table); a++) {
					UResourceBundle *alias = ures_getByIndex(alias_table, a, nullptr, &status);
					check(status, "type alias");
					if (ResourceString(alias) == target) {
						string from = ures_getKey(alias);
						key.types.emplace_back(alias_table == aliases ? slashes(from) : from, legacy);
					}
					ures_close(alias);
				}
			}
		}
		ures_close(types);
		ures_close(aliases);
		ures_close(bcp_aliases);
		keys.push_back(std::move(key));
	}
	ures_close(bcp_type_alias);
	ures_close(type_alias);
	ures_close(type_map);
	ures_close(key_map);
	ures_close(data);
	out.U32(uint32_t(keys.size()));
	for (auto &key : keys) {
		out.Name(key.legacy);
		out.Name(key.bcp);
		out.U8(key.special);
		out.U32(uint32_t(key.types.size()));
		for (auto &type : key.types) {
			out.Name(type.first);
			out.Name(type.second);
		}
	}
}

void Export(const char *path, const std::set<string> &types) {
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

	// the bundles of the coll tree
	vector<string> bundle_names;
	for (auto &item : items) {
		const string &name = item.name;
		if (name.compare(0, coll_prefix.size(), coll_prefix) != 0 || name.size() < 4 ||
		    name.compare(name.size() - 4, 4, ".res") != 0) {
			continue;
		}
		string bundle = name.substr(coll_prefix.size(), name.size() - coll_prefix.size() - 4);
		if (bundle != "res_index" && bundle != "pool") {
			bundle_names.push_back(bundle);
		}
	}
	// the tailoring each bundle loads by default and for each collation type
	// (`Collator::createInstance(Locale(bundle@collation=type))`), the types only where it
	// differs from the default
	Tailorings tailorings;
	struct Bundle {
		string name;
		uint32_t index;
		vector<std::pair<string, uint32_t>> types;
	};
	vector<Bundle> bundles;
	for (auto &bundle : bundle_names) {
		Bundle entry {bundle, tailorings.Add(CloneBinary(*CollatorFor(bundle, ""))), {}};
		for (auto &type : types) {
			// some types have rules but no binary data: U_MISSING_RESOURCE_ERROR
			UErrorCode status = U_ZERO_ERROR;
			// (a bundle ending with _, like de_, is reached from IDs with a variant: in
			// de_@collation=x, uloc_getName would take the keywords for the variant)
			string id = bundle.back() == '_' ? bundle + "_ZZZZ" : bundle;
			icu::Locale locale((id + "@collation=" + type).c_str());
			std::unique_ptr<icu::Collator> collator(icu::Collator::createInstance(locale, status));
			uint32_t index = MISSING_RESOURCE;
			if (status != U_MISSING_RESOURCE_ERROR) {
				check(status, "createInstance " + bundle + "@collation=" + type);
				index = tailorings.Add(CloneBinary(*collator));
			}
			if (index != entry.index) {
				entry.types.emplace_back(type, index);
			}
		}
		bundles.push_back(std::move(entry));
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
	out.U32(uint32_t(types.size()));
	for (auto &type : types) {
		out.Name(type);
	}
	out.U32(uint32_t(bundles.size()));
	for (auto &bundle : bundles) {
		out.Name(bundle.name);
		out.U32(bundle.index);
		out.U32(uint32_t(bundle.types.size()));
		for (auto &type : bundle.types) {
			out.Name(type.first);
			out.U32(type.second);
		}
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

	for (UChar32 high = 0; high < 0x10000; high += 0x100) {
		uint8_t bits = 0;
		for (int32_t bit = 0; bit < 8; bit++) {
			if (nfc_impl->singleLeadMightHaveNonZeroFCD16(high | (bit << 5))) {
				bits |= uint8_t(1 << bit);
			}
		}
		out.U8(bits);
	}

	// the three-letter codes uloc_getName replaces
	auto get_name = [](const string &id) {
		char name[ULOC_FULLNAME_CAPACITY];
		UErrorCode status = U_ZERO_ERROR;
		int32_t length = uloc_getName(id.c_str(), name, sizeof(name), &status);
		check(status, "uloc_getName " + id);
		return string(name, length);
	};
	vector<std::pair<string, string>> languages, countries;
	const string letters = "abcdefghijklmnopqrstuvwxyz";
	const string alphanumerics = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
	for (char a : letters) {
		for (char b : letters) {
			for (char c : letters) {
				string code {a, b, c};
				string name = get_name(code);
				if (name != code && code != "und") {
					languages.emplace_back(code, name);
				}
			}
		}
	}
	for (char a : alphanumerics) {
		for (char b : alphanumerics) {
			for (char c : alphanumerics) {
				string code {a, b, c};
				string name = get_name("en_" + code);
				if (name != "en_" + code) {
					if (name.compare(0, 3, "en_") != 0) {
						fail("unexpected uloc_getName of en_" + code);
					}
					countries.emplace_back(code, name.substr(3));
				}
			}
		}
	}
	for (auto *table : {&languages, &countries}) {
		out.U32(uint32_t(table->size()));
		for (auto &entry : *table) {
			out.Name(entry.first);
			out.Name(entry.second);
		}
	}

	// the names of the scripts (`u_getPropertyValueEnum(UCHAR_SCRIPT, name)`)
	vector<std::pair<string, int32_t>> scripts;
	for (int32_t code = 0; code < u_getIntPropertyMaxValue(UCHAR_SCRIPT) + 1; code++) {
		for (int32_t choice = 0;; choice++) {
			const char *script = u_getPropertyValueName(UCHAR_SCRIPT, code, UPropertyNameChoice(choice));
			if (!script) {
				if (choice == 0) {
					continue;
				}
				break;
			}
			scripts.emplace_back(script, code);
		}
	}
	out.U32(uint32_t(scripts.size()));
	for (auto &script : scripts) {
		out.Name(script.first);
		out.U32(uint32_t(script.second));
	}

	// keyTypeData, as uloc_keytype.cpp's initFromResourceBundle reads it
	KeyTypeData(out);

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
	std::cerr << "icu-collation-data: " << bundles.size() << " bundles, " << types.size() << " types, "
	          << tailorings.binaries.size()
	          << " tailorings, " << runs.size() << " FCD16 runs, " << decompositions.size()
	          << " decompositions, " << languages.size() << " + " << countries.size() << " locale codes, "
	          << count << " collations, " << out.bytes.size() << " bytes\n";
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
	if (argc >= 3 && string(argv[1]) == "export") {
		Export(argv[2], std::set<string>(argv + 3, argv + argc));
	} else if (argc == 2 && string(argv[1]) == "types") {
		Types();
	} else if (argc == 3 && string(argv[1]) == "keys") {
		Keys(argv[2]);
	} else {
		fail("usage: icu-collation-data types | export <out.bin> <types...> | keys <collation>");
	}
	return 0;
}
