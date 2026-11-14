
SELECT
	score,
	left_tbl,
	right_tbl,
FROM
	(SELECT * FROM query_table(left_table::VARCHAR)) as left_tbl,
	(
		SELECT
			struct_pack(*columns([x for x in (matches.*) if x != 'score'])) as right_tbl,
			matches.score as score
		FROM (
			SELECT (
				unnest(
					CASE WHEN metric = 'l2sq' OR metric = 'l2'
						THEN min_by(tbl, tbl.score, k)
						ELSE max_by(tbl, tbl.score, k)
					END,
					max_depth := 2
				)
			) as result
			FROM (
				SELECT
				*,
				CASE
					WHEN metric = 'l2sq' OR metric = 'l2'
					THEN array_distance(left_col, right_col)
					WHEN metric = 'cosine' OR metric = 'cos'
					THEN array_cosine_similarity(left_col, right_col)
					WHEN metric = 'ip'
					THEN array_inner_product(left_col, right_col)
					ELSE error('Unknown metric')
				END as score,
				FROM query_table(right_table::VARCHAR)
			) as tbl
		) as matches
	)
