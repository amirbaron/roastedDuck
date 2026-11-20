//! Bundled Avro object container reader and COPY writer.
mod container;
mod read;
mod write;

pub(crate) fn register(catalog: &roastedduck_catalog::Catalog) -> roastedduck_common::Result<()> {
    catalog.register_table_functions(read::functions())?;
    catalog.register_copy_function(write::copy_function())
}
