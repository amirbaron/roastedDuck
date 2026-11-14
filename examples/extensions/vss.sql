-- Run against :memory:. Persisted HNSW creation is not implemented.
LOAD vss;
CREATE TABLE items(id INTEGER PRIMARY KEY, embedding FLOAT[3]);
INSERT INTO items VALUES (1, [1, 0, 0]), (2, [0, 1, 0]), (3, [0, 0, 1]);
CREATE INDEX items_hnsw ON items USING HNSW(embedding) WITH (metric='cosine');
SET hnsw_ef_search=200;
SELECT id, array_cosine_distance(embedding, [1, 0, 0]::FLOAT[3]) AS distance
FROM items
ORDER BY distance
LIMIT 1;
-- Inspect acceleration with EXPLAIN before the SELECT above.
-- SELECT * FROM pragma_hnsw_index_info();
-- PRAGMA hnsw_compact_index('items_hnsw');
-- DROP INDEX items_hnsw;
