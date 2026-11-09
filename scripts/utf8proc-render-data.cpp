// Offline exporter for DuckDB 1.5.6's utf8proc rendering properties.
// Only the data is embedded in roastedduck; this executable is never shipped.
#include "utf8proc.hpp"
#include <cstdint>
#include <fstream>
#include <iostream>
int main(int argc, char **argv) {
    if (argc != 2) return 2;
    std::ofstream out(argv[1], std::ios::binary);
    int previous = -1;
    for (int32_t cp = 0; cp <= 0x110000; ++cp) {
        auto p = duckdb::utf8proc_get_property(cp);
        uint16_t value = p->charwidth | (p->boundclass << 2) | (p->indic_conjunct_break << 7);
        if (value == previous && cp != 0x110000) continue;
        previous = value;
        for (int shift = 0; shift < 32; shift += 8) out.put(char(uint32_t(cp) >> shift));
        out.put(char(value)); out.put(char(value >> 8));
    }
    if (!out) return 1;
}
