
SELECT
	right_tbl as matches,
FROM
	(
		SELECT (
			CASE WHEN metric = 'l2sq' OR metric = 'l2'
				THEN min_by({'score': score, 'row': t}, score, k)
				ELSE max_by({'score': score, 'row': t}, score, k)
			END
		) as right_tbl
		FROM (
			SELECT
			CASE
				WHEN metric = 'l2sq' OR metric = 'l2'
				THEN array_distance(left_col, right_col)
				WHEN metric = 'cosine' OR metric = 'cos'
				THEN array_cosine_similarity(left_col, right_col)
				WHEN metric = 'ip'
				THEN array_inner_product(left_col, right_col)
				ELSE error('Unknown metric')
			END as score,
			tbl as t,
			FROM (SELECT * FROM query_table(right_table::VARCHAR)) as tbl
		)
	)
