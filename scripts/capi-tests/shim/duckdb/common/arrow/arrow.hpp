// Stand-in for DuckDB's arrow.hpp: the Arrow C data interface structs (arrow/c/abi.h) with
// the `Init()` helpers DuckDB adds to them, and `ArrowSchemaWrapper` (arrow_wrapper.hpp).
#pragma once

#include <cstdint>

#ifndef ARROW_C_DATA_INTERFACE
#define ARROW_C_DATA_INTERFACE

struct ArrowSchema {
	const char *format;
	const char *name;
	const char *metadata;
	int64_t flags;
	int64_t n_children;
	struct ArrowSchema **children;
	struct ArrowSchema *dictionary;
	void (*release)(struct ArrowSchema *);
	void *private_data;

	void Init() {
		flags = 0;
		n_children = 0;
		release = nullptr;
		private_data = nullptr;
	}
};

struct ArrowArray {
	int64_t length;
	int64_t null_count;
	int64_t offset;
	int64_t n_buffers;
	int64_t n_children;
	const void **buffers;
	struct ArrowArray **children;
	struct ArrowArray *dictionary;
	void (*release)(struct ArrowArray *);
	void *private_data;

	void Init() {
		length = 0;
		null_count = 0;
		offset = 0;
		n_buffers = 0;
		n_children = 0;
		release = nullptr;
		private_data = nullptr;
	}
};

#endif

#ifndef ARROW_C_STREAM_INTERFACE
#define ARROW_C_STREAM_INTERFACE

struct ArrowArrayStream {
	int (*get_schema)(struct ArrowArrayStream *, struct ArrowSchema *out);
	int (*get_next)(struct ArrowArrayStream *, struct ArrowArray *out);
	const char *(*get_last_error)(struct ArrowArrayStream *);
	void (*release)(struct ArrowArrayStream *);
	void *private_data;
};

#endif

namespace duckdb {

//! An `ArrowSchema` released when it goes out of scope.
class ArrowSchemaWrapper {
public:
	ArrowSchema arrow_schema;

	ArrowSchemaWrapper() {
		arrow_schema.release = nullptr;
	}
	~ArrowSchemaWrapper() {
		if (arrow_schema.release) {
			arrow_schema.release(&arrow_schema);
		}
	}
};

} // namespace duckdb
