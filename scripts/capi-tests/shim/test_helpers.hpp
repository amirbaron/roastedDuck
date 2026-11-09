// Stand-in for DuckDB's test/include/test_helpers.hpp when the C API tests are built
// against roastedduck's libduckdb: the tests must only reach the engine through duckdb.h, so
// the C++ utilities they use (strings, containers, exceptions, a few value helpers) are
// provided here as header-only test code. Test-only; never shipped.
#pragma once

#include "duckdb.h"

#include <atomic>
#include <cmath>
#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <map>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

#define D_ASSERT(condition) ((void)0)

namespace duckdb {

using std::make_pair;
using std::move;
using std::pair;
using std::string;
using std::to_string;
template <class T>
using vector = std::vector<T>;
template <class T, class D = std::default_delete<T>>
using unique_ptr = std::unique_ptr<T, D>;
template <class T>
using shared_ptr = std::shared_ptr<T>;
using std::map;
using std::mutex;
using std::unordered_map;
using std::unordered_set;
template <class T>
using child_list_t = vector<pair<string, T>>;
typedef uint32_t sel_t;
typedef uint8_t data_t;
typedef data_t *data_ptr_t;
typedef const data_t *const_data_ptr_t;

template <class T, class... ARGS>
unique_ptr<T> make_uniq(ARGS &&...args) {
	return unique_ptr<T>(new T(std::forward<ARGS>(args)...));
}

struct DConstants {
	static constexpr const idx_t INVALID_INDEX = idx_t(-1);
};

class Exception : public std::runtime_error {
public:
	explicit Exception(const string &message) : std::runtime_error(message) {
	}
};

class NotImplementedException : public Exception {
public:
	explicit NotImplementedException(const string &message) : Exception(message) {
	}
};

class InvalidInputException : public Exception {
public:
	explicit InvalidInputException(const string &message) : Exception(message) {
	}
};

struct StringUtil {
 static string Replace(string value,const string &from,const string &to){if(from.empty())return value;size_t pos=0;while((pos=value.find(from,pos))!=string::npos){value.replace(pos,from.size(),to);pos+=to.size();}return value;}
	static bool Contains(const string &haystack, const string &needle) {
		return haystack.find(needle) != string::npos;
	}
	static bool Equals(const char *left, const char *right) {
		if (left == right) {
			return true;
		}
		if (!left || !right) {
			return false;
		}
		return strcmp(left, right) == 0;
	}
	static bool Equals(const char *left, const string &right) {
		return Equals(left, right.c_str());
	}
	static string Lower(string text) {
		for (auto &c : text) {
			c = char(tolower(c));
		}
		return text;
	}
	static bool CIEquals(const string &left, const string &right) {
		return Lower(left) == Lower(right);
	}
	//! printf-style formatting of %s / %d / %llu arguments, as used by the tests.
	template <class... ARGS>
	static string Format(const string &format, ARGS... args) {
		return FormatInternal(format, {ToText(args)...});
	}

private:
	static string ToText(const string &value) {
		return value;
	}
	static string ToText(const char *value) {
		return value;
	}
	template <class T>
	static string ToText(T value) {
		return to_string(value);
	}
	static string FormatInternal(const string &format, std::initializer_list<string> values) {
		string result;
		auto value = values.begin();
		for (size_t i = 0; i < format.size(); i++) {
			if (format[i] != '%' || i + 1 >= format.size()) {
				result += format[i];
				continue;
			}
			if (format[i + 1] == '%') {
				result += '%';
				i++;
				continue;
			}
			// Skip flags and length modifiers up to the conversion character.
			size_t end = i + 1;
			while (end + 1 < format.size() && !strchr("sdiufxc", format[end])) {
				end++;
			}
			if (value != values.end()) {
				result += *value++;
			}
			i = end;
		}
		return result;
	}
};

#ifndef STANDARD_VECTOR_SIZE
#define STANDARD_VECTOR_SIZE 2048
#endif

class InternalException : public Exception {
public:
	explicit InternalException(const string &message) : Exception(message) {
	}
};

//! A 128-bit integer split like DuckDB's `hugeint_t` (lower bits, signed upper bits).
struct hugeint_t {
	uint64_t lower = 0;
	int64_t upper = 0;

	hugeint_t() = default;
	hugeint_t(int64_t value) // NOLINT: implicit like DuckDB's
	    : lower(static_cast<uint64_t>(value)), upper(value < 0 ? -1 : 0) {
	}
	static hugeint_t FromInt128(__int128 value) {
		hugeint_t result;
		result.lower = static_cast<uint64_t>(value);
		result.upper = static_cast<int64_t>(value >> 64);
		return result;
	}
	__int128 ToInt128() const {
		return (static_cast<__int128>(upper) << 64) | static_cast<__int128>(lower);
	}
	hugeint_t operator-(const hugeint_t &other) const {
		return FromInt128(ToInt128() - other.ToInt128());
	}
	hugeint_t operator-() const {
		return FromInt128(-ToInt128());
	}
};

template <class T>
struct NumericLimits {
	static constexpr T Minimum() {
		return std::numeric_limits<T>::lowest();
	}
	static constexpr T Maximum() {
		return std::numeric_limits<T>::max();
	}
};

//! The profiling metrics the tests name (DuckDB's `MetricType`), spelled like
//! `EnumUtil::ToString` renders them.
enum class MetricType {
	BLOCKED_THREAD_TIME,
	EXTRA_INFO,
	LATENCY,
	OPERATOR_CARDINALITY,
	OPERATOR_NAME,
	OPERATOR_ROWS_SCANNED,
	OPERATOR_ROW_GROUPS_SCANNED,
	OPERATOR_TIMING,
	OPERATOR_TOTAL_ROW_GROUPS_TO_SCAN,
	OPERATOR_TYPE,
	QUERY_NAME,
	ROWS_RETURNED,
};

struct EnumUtil {
	static string ToString(MetricType metric) {
		static const char *const NAMES[] = {"BLOCKED_THREAD_TIME",
		                                    "EXTRA_INFO",
		                                    "LATENCY",
		                                    "OPERATOR_CARDINALITY",
		                                    "OPERATOR_NAME",
		                                    "OPERATOR_ROWS_SCANNED",
		                                    "OPERATOR_ROW_GROUPS_SCANNED",
		                                    "OPERATOR_TIMING",
		                                    "OPERATOR_TOTAL_ROW_GROUPS_TO_SCAN",
		                                    "OPERATOR_TYPE",
		                                    "QUERY_NAME",
		                                    "ROWS_RETURNED"};
		return NAMES[static_cast<int>(metric)];
	}
};

//! DuckDB's `NumericCast` (numeric_utils.hpp), unchecked.
template <class TO, class FROM>
TO NumericCast(FROM value) {
	return static_cast<TO>(value);
}

struct Printer {
	static void Print(const string &message) {
		fprintf(stderr, "%s\n", message.c_str());
	}
};

//! The appender's flush threshold (`BaseAppender::DEFAULT_FLUSH_COUNT`).
struct BaseAppender {
	static constexpr const idx_t DEFAULT_FLUSH_COUNT = STANDARD_VECTOR_SIZE * 100ULL;
};

struct Hugeint {
	//! 10^0 .. 10^38.
	struct PowersOfTen {
		hugeint_t operator[](size_t exponent) const {
			__int128 value = 1;
			for (size_t i = 0; i < exponent; i++) {
				value *= 10;
			}
			return hugeint_t::FromInt128(value);
		}
	};
	static constexpr PowersOfTen POWERS_OF_TEN {};
	static bool TryConvert(int64_t value, hugeint_t &result) {
		result = hugeint_t(value);
		return true;
	}
};

//! `dtime_ns_t`: nanoseconds since midnight.
struct dtime_ns_t {
	explicit dtime_ns_t(int64_t nanos) : micros(nanos) {
	}
	int64_t micros;
};

//! The few `duckdb::Value` constructors the tests format expected results with, rendered
//! like DuckDB's `Date::ToString` / `Time::ToString`.
class Value {
public:
	static Value DATE(int32_t year, int32_t month, int32_t day) {
		char buffer[64];
		snprintf(buffer, sizeof(buffer), "%04d-%02d-%02d", year, month, day);
		return Value(buffer);
	}
	static Value TIME(int32_t hour, int32_t minute, int32_t second, int32_t micros) {
		return Value(FormatTime(hour, minute, second, micros, 6));
	}
	static Value TIME_NS(dtime_ns_t time) {
		auto nanos = time.micros;
		auto seconds = nanos / 1000000000;
		return Value(FormatTime(int32_t(seconds / 3600), int32_t(seconds / 60 % 60), int32_t(seconds % 60),
		                        nanos % 1000000000, 9));
	}
	const string &ToString() const {
		return text;
	}

private:
	explicit Value(string text_p) : text(std::move(text_p)) {
	}
	static string FormatTime(int32_t hour, int32_t minute, int32_t second, int64_t fraction, int digits) {
		char buffer[64];
		snprintf(buffer, sizeof(buffer), "%02d:%02d:%02d", hour, minute, second);
		string result = buffer;
		if (fraction != 0) {
			snprintf(buffer, sizeof(buffer), ".%0*lld", digits, static_cast<long long>(fraction));
			string decimals = buffer;
			while (decimals.back() == '0') {
				decimals.pop_back();
			}
			result += decimals;
		}
		return result;
	}
	string text;
};

//! DuckDB's `ApproxEqual` (operator/comparison_operators.hpp): within 1% plus 1e-8.
inline bool ApproxEqual(double left, double right) {
	if (std::isnan(left) && std::isnan(right)) {
		return true;
	}
	if (!std::isfinite(left) || !std::isfinite(right)) {
		return left == right;
	}
	auto epsilon = std::fabs(right) * 0.01 + 0.00000001;
	return std::fabs(left - right) <= epsilon;
}

inline bool ApproxEqual(float left, float right) {
	return ApproxEqual(double(left), double(right));
}

//! A test directory per process, created on first use (DuckDB's `TestDirectoryPath`).
string TestDirectoryPath();
//! `TestDirectoryPath()` joined with `suffix` (DuckDB's `TestCreatePath`).
string TestCreatePath(string suffix);
void TestDeleteFile(string path);
void DeleteDatabase(string path);

} // namespace duckdb

#define REQUIRE_NO_FAIL(result) REQUIRE(NO_FAIL((result)))
#define REQUIRE_FAIL(result)    REQUIRE((result)->HasError())
