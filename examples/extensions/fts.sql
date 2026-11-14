-- Run against a new database. FTS data can be saved in a database file.
LOAD fts;
CREATE TABLE documents(id INTEGER PRIMARY KEY, body VARCHAR);
INSERT INTO documents VALUES
    (1, 'a red fox running through the forest'),
    (2, 'a blue bird above the ocean');
PRAGMA create_fts_index('documents', 'id', 'body');
SELECT id, body, fts_main_documents.match_bm25(id, 'fox') AS score
FROM documents
WHERE score IS NOT NULL
ORDER BY score DESC, id;
-- After changing documents, refresh the snapshot with:
-- PRAGMA create_fts_index('documents', 'id', 'body', overwrite=true);
-- PRAGMA drop_fts_index('documents');
