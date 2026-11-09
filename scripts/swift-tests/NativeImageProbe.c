/* Test-only Linux loader inspection; never part of the shipped engine/client. */
#define _GNU_SOURCE
#include "NativeImageProbe.h"
#include <dlfcn.h>
#include <link.h>

const char *roastedduck_test_function_image(const void *address) {
    Dl_info info;
    return dladdr(address, &info) ? info.dli_fname : NULL;
}
struct ImageQuery { size_t wanted; size_t count; const char *name; };
static int image_callback(struct dl_phdr_info *info, size_t size, void *opaque) {
    (void)size;
    struct ImageQuery *query = (struct ImageQuery *)opaque;
    if (query->count++ == query->wanted) { query->name = info->dlpi_name; return 1; }
    return 0;
}
size_t roastedduck_test_image_count(void) {
    struct ImageQuery query = {(size_t)-1, 0, NULL};
    dl_iterate_phdr(image_callback, &query);
    return query.count;
}
const char *roastedduck_test_image_name(size_t index) {
    struct ImageQuery query = {index, 0, NULL};
    dl_iterate_phdr(image_callback, &query);
    return query.name;
}
