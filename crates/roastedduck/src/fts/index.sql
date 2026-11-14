-- From duckdb-fts 6814ec9a7d5fd63500176507262b0dbf7cea0095; see LICENSE.

        CREATE TABLE %fts_schema%.docs AS (
            SELECT rowid AS docid,
                   "%input_id%" AS name
            FROM %input_table%
        );

	    CREATE TABLE %fts_schema%.fields (fieldid BIGINT, field VARCHAR);
	    INSERT INTO %fts_schema%.fields VALUES %field_values%;

        CREATE TABLE %fts_schema%.terms AS
        WITH tokenized AS (
            %union_fields_query%
        ),
	    stemmed_stopped AS (
            SELECT stem(t.w, '%stemmer%') AS term,
	               t.docid AS docid,
                   t.fieldid AS fieldid
	        FROM tokenized AS t
	        WHERE t.w NOT NULL
              AND len(t.w) > 0
	          AND t.w NOT IN (SELECT sw FROM %fts_schema%.stopwords)
        )
	    SELECT ss.term,
	           ss.docid,
	           ss.fieldid
        FROM stemmed_stopped AS ss;

        ALTER TABLE %fts_schema%.docs ADD len BIGINT;
        UPDATE %fts_schema%.docs d
        SET len = (
            SELECT count(term)
            FROM %fts_schema%.terms AS t
            WHERE t.docid = d.docid
        );

        CREATE TABLE %fts_schema%.dict AS
        WITH distinct_terms AS (
            SELECT DISTINCT term
            FROM %fts_schema%.terms
            ORDER BY docid, term
        )
        SELECT row_number() OVER () - 1 AS termid,
               dt.term
        FROM distinct_terms AS dt;

        ALTER TABLE %fts_schema%.terms ADD termid BIGINT;
        UPDATE %fts_schema%.terms t
        SET termid = (
            SELECT termid
            FROM %fts_schema%.dict d
            WHERE t.term = d.term
        );
        ALTER TABLE %fts_schema%.terms DROP term;

        ALTER TABLE %fts_schema%.dict ADD df BIGINT;
        UPDATE %fts_schema%.dict d
        SET df = (
            SELECT count(distinct docid)
            FROM %fts_schema%.terms t
            WHERE d.termid = t.termid
            GROUP BY termid
        );

        CREATE TABLE %fts_schema%.stats AS (
            SELECT COUNT(docs.docid) AS num_docs,
                   SUM(docs.len) / COUNT(docs.len) AS avgdl
            FROM %fts_schema%.docs AS docs
        );

        CREATE MACRO %fts_schema%.match_bm25(docname, query_string, fields := NULL, k := 1.2, b := 0.75, conjunctive := false) AS (
            WITH tokens AS (
                SELECT DISTINCT stem(unnest(%fts_schema%.tokenize(query_string)), '%stemmer%') AS t
            ),
            fieldids AS (
                SELECT fieldid
                FROM %fts_schema%.fields
                WHERE CASE WHEN fields IS NULL THEN 1 ELSE field IN (SELECT * FROM (SELECT UNNEST(string_split(fields, ','))) AS fsq) END
            ),
            qtermids AS (
                SELECT termid
                FROM %fts_schema%.dict AS dict,
                     tokens
                WHERE dict.term = tokens.t
            ),
            qterms AS (
                SELECT termid,
                       docid
                FROM %fts_schema%.terms AS terms
                WHERE CASE WHEN fields IS NULL THEN 1 ELSE fieldid IN (SELECT * FROM fieldids) END
                  AND termid IN (SELECT qtermids.termid FROM qtermids)
            ),
			term_tf AS (
				SELECT termid,
				   	   docid,
                       COUNT(*) AS tf
				FROM qterms
				GROUP BY docid,
						 termid
			),
			cdocs AS (
				SELECT docid
				FROM qterms
				GROUP BY docid
				HAVING CASE WHEN conjunctive THEN COUNT(DISTINCT termid) = (SELECT COUNT(*) FROM tokens) ELSE 1 END
			),
            subscores AS (
                SELECT docs.docid,
                       len,
                       term_tf.termid,
                       tf,
                       df,
                       (log(((SELECT num_docs FROM %fts_schema%.stats) - df + 0.5) / (df + 0.5) + 1) * ((tf * (k + 1)/(tf + k * (1 - b + b * (len / (SELECT avgdl FROM %fts_schema%.stats))))))) AS subscore
                FROM term_tf,
					 cdocs,
					 %fts_schema%.docs AS docs,
					 %fts_schema%.dict AS dict
				WHERE term_tf.docid = cdocs.docid
				  AND term_tf.docid = docs.docid
                  AND term_tf.termid = dict.termid
            ),
			scores AS (
				SELECT docid,
					   sum(subscore) AS score
				FROM subscores
				GROUP BY docid
			)
            SELECT score
            FROM scores,
				 %fts_schema%.docs AS docs
            WHERE scores.docid = docs.docid
              AND docs.name = docname
        );
    