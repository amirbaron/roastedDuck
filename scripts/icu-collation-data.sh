#!/usr/bin/env bash
# Generates crates/icu/data/collation.postcard: the ICU4X collation data of the
# locales whose tailoring ICU4X's compiled data lacks but DuckDB's ICU data has.
# Requires `cargo install icu4x-datagen --version 2.3.0` (downloads CLDR and ICU export data).
set -euo pipefail
cd "$(dirname "$0")/.."
icu4x-datagen --format blob --overwrite \
    --markers CollationDiacriticsV1 CollationJamoV1 CollationMetadataV1 CollationReorderingV1 \
        CollationRootV1 CollationSpecialPrimariesV1 CollationTailoringV1 \
        NormalizerNfdDataV1 NormalizerNfdTablesV1 \
    --locales haw kl lkt se smn \
    --out crates/icu/data/collation.postcard
