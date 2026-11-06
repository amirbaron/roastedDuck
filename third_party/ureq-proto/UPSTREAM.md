# ureq-proto response metadata patch

Based on published ureq-proto0.6.5. Original crate SHA-256:
841583fc36089d7d59419568ae157f43a5a03a2e5a81ba87c77d1596ef46e922

Retains original response reason and header spelling/bytes in each completed response
extension before HeaderMap normalizes names. Required for DuckDB HTTPException metadata
and Python response attributes. Existing protocol parsing/validation and streaming
bodies remain unchanged. Original licenses retained. Cargo registry source is untouched.
No table/data response cache is introduced.
