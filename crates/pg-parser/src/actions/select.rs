//! Semantic actions for the rules in `grammar/statements/select.y`.

use super::prelude::*;

/// ```text
/// select_no_parens:
///       simple_select
///     | select_clause sort_clause
///     | select_clause opt_sort_clause for_locking_clause opt_select_limit
///     | select_clause opt_sort_clause select_limit opt_for_locking_clause
///     | with_clause select_clause
///     | with_clause select_clause sort_clause
///     | with_clause select_clause opt_sort_clause for_locking_clause opt_select_limit
///     | with_clause select_clause opt_sort_clause select_limit opt_for_locking_clause
/// ```
pub(super) fn select_no_parens<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // select_clause sort_clause
        1 => {
            let mut select = rhs.required_node(1)?;
            insert_select_options(&mut select, rhs.list(2)?, List::new(), SelectLimit::none(), None)?;
            Ok(SemValue::node(select))
        }
        // select_clause opt_sort_clause for_locking_clause opt_select_limit
        2 => {
            let mut select = rhs.required_node(1)?;
            insert_select_options(
                &mut select,
                rhs.list(2)?,
                rhs.list(3)?,
                *rhs.select_limit(4)?,
                None,
            )?;
            Ok(SemValue::node(select))
        }
        // select_clause opt_sort_clause select_limit opt_for_locking_clause
        3 => {
            let mut select = rhs.required_node(1)?;
            insert_select_options(
                &mut select,
                rhs.list(2)?,
                rhs.list(4)?,
                *rhs.select_limit(3)?,
                None,
            )?;
            Ok(SemValue::node(select))
        }
        // with_clause select_clause
        4 => {
            let mut select = rhs.required_node(2)?;
            insert_select_options(
                &mut select,
                List::new(),
                List::new(),
                SelectLimit::none(),
                rhs.ptr::<WithClause>(1)?,
            )?;
            Ok(SemValue::node(select))
        }
        // with_clause select_clause sort_clause
        5 => {
            let mut select = rhs.required_node(2)?;
            insert_select_options(
                &mut select,
                rhs.list(3)?,
                List::new(),
                SelectLimit::none(),
                rhs.ptr::<WithClause>(1)?,
            )?;
            Ok(SemValue::node(select))
        }
        // with_clause select_clause opt_sort_clause for_locking_clause opt_select_limit
        6 => {
            let mut select = rhs.required_node(2)?;
            let with = rhs.ptr::<WithClause>(1)?;
            insert_select_options(
                &mut select,
                rhs.list(3)?,
                rhs.list(4)?,
                *rhs.select_limit(5)?,
                with,
            )?;
            Ok(SemValue::node(select))
        }
        // with_clause select_clause opt_sort_clause select_limit opt_for_locking_clause
        7 => {
            let mut select = rhs.required_node(2)?;
            let with = rhs.ptr::<WithClause>(1)?;
            insert_select_options(
                &mut select,
                rhs.list(3)?,
                rhs.list(5)?,
                *rhs.select_limit(4)?,
                with,
            )?;
            Ok(SemValue::node(select))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_select:
///       SELECT opt_all_clause opt_target_list_opt_comma
///     | /* empty */
/// ```
pub(super) fn opt_select<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => {
            let star = AStar { ..Default::default() };
            Ok(SemValue::List(vec![star.into()]))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// simple_select:
///       SELECT opt_all_clause opt_target_list_opt_comma into_clause from_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
///     | SELECT distinct_clause target_list_opt_comma into_clause from_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
///     | FROM from_list opt_select into_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
///     | FROM from_list SELECT distinct_clause target_list_opt_comma into_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
///     | values_clause_opt_comma
///     | TABLE relation_expr
///     | select_clause UNION all_or_distinct by_name select_clause
///     | select_clause UNION all_or_distinct select_clause
///     | select_clause INTERSECT all_or_distinct select_clause
///     | select_clause EXCEPT all_or_distinct select_clause
///     | pivot_keyword table_ref USING target_list_opt_comma
///     | pivot_keyword table_ref USING target_list_opt_comma GROUP_P BY name_list_opt_comma_opt_bracket
///     | pivot_keyword table_ref GROUP_P BY name_list_opt_comma_opt_bracket
///     | pivot_keyword table_ref ON pivot_column_list
///     | pivot_keyword table_ref ON pivot_column_list GROUP_P BY name_list_opt_comma_opt_bracket
///     | pivot_keyword table_ref ON pivot_column_list USING target_list_opt_comma
///     | pivot_keyword table_ref ON pivot_column_list USING target_list_opt_comma GROUP_P BY name_list_opt_comma_opt_bracket
///     | unpivot_keyword table_ref ON target_list_opt_comma INTO NAME_P name value_or_values name_list_opt_comma_opt_bracket
///     | unpivot_keyword table_ref ON target_list_opt_comma
/// ```
pub(super) fn simple_select<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SELECT opt_all_clause opt_target_list_opt_comma into_clause from_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
        0 => {
            let n = SelectStmt {
                target_list: rhs.list(3)?,
                into_clause: rhs.ptr::<IntoClause>(4)?,
                from_clause: rhs.list(5)?,
                where_clause: rhs.node(6)?,
                group_clause: rhs.list(7)?,
                having_clause: rhs.node(8)?,
                window_clause: rhs.list(9)?,
                qualify_clause: rhs.node(10)?,
                sample_options: rhs.node(11)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // SELECT distinct_clause target_list_opt_comma into_clause from_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
        1 => Ok(SemValue::node(SelectStmt {
            distinct_clause: rhs.opt_list(2)?,
            target_list: rhs.list(3)?,
            into_clause: rhs.ptr::<IntoClause>(4)?,
            from_clause: rhs.list(5)?,
            where_clause: rhs.node(6)?,
            group_clause: rhs.list(7)?,
            having_clause: rhs.node(8)?,
            window_clause: rhs.list(9)?,
            qualify_clause: rhs.node(10)?,
            sample_options: rhs.node(11)?,
            ..Default::default()
        })),
        // FROM from_list opt_select into_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
        2 => {
            let n = SelectStmt {
                target_list: rhs.list(3)?,
                from_clause: rhs.list(2)?,
                into_clause: rhs.ptr::<IntoClause>(4)?,
                where_clause: rhs.node(5)?,
                group_clause: rhs.list(6)?,
                having_clause: rhs.node(7)?,
                window_clause: rhs.list(8)?,
                qualify_clause: rhs.node(9)?,
                sample_options: rhs.node(10)?,
                from_first: true,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // FROM from_list SELECT distinct_clause target_list_opt_comma into_clause where_clause group_clause having_clause window_clause qualify_clause sample_clause
        3 => Ok(SemValue::node(SelectStmt {
            target_list: rhs.list(5)?,
            distinct_clause: rhs.opt_list(4)?,
            from_clause: rhs.list(2)?,
            into_clause: rhs.ptr::<IntoClause>(6)?,
            where_clause: rhs.node(7)?,
            group_clause: rhs.list(8)?,
            having_clause: rhs.node(9)?,
            window_clause: rhs.list(10)?,
            qualify_clause: rhs.node(11)?,
            sample_options: rhs.node(12)?,
            from_first: true,
            ..Default::default()
        })),
        // TABLE relation_expr
        5 => {
            let mut cr = ColumnRef { ..Default::default() };
            let mut rt = ResTarget { ..Default::default() };
            let mut n = SelectStmt { ..Default::default() };
            cr.fields = vec![AStar::default().into()];
            cr.location = -1;
            rt.name = None;
            rt.indirection = List::new();
            rt.val = Some(cr.into());
            rt.location = -1;
            n.target_list = vec![rt.into()];
            n.from_clause = vec![rhs.required_node(2)?];
            Ok(SemValue::node(n))
        }
        // select_clause UNION all_or_distinct by_name select_clause
        6 => Ok(SemValue::node(make_set_op(
            SetOperation::UnionByName,
            rhs.bool(3)?,
            rhs.node(1)?,
            rhs.node(5)?,
        ))),
        // select_clause UNION all_or_distinct select_clause
        7 => Ok(SemValue::node(make_set_op(
            SetOperation::Union,
            rhs.bool(3)?,
            rhs.node(1)?,
            rhs.node(4)?,
        ))),
        // select_clause INTERSECT all_or_distinct select_clause
        8 => Ok(SemValue::node(make_set_op(
            SetOperation::Intersect,
            rhs.bool(3)?,
            rhs.node(1)?,
            rhs.node(4)?,
        ))),
        // select_clause EXCEPT all_or_distinct select_clause
        9 => Ok(SemValue::node(make_set_op(
            SetOperation::Except,
            rhs.bool(3)?,
            rhs.node(1)?,
            rhs.node(4)?,
        ))),
        // pivot_keyword table_ref USING target_list_opt_comma
        10 => {
            let mut res = SelectStmt { ..Default::default() };
            let n = PivotStmt {
                source: rhs.node(2)?,
                aggrs: rhs.list(4)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // pivot_keyword table_ref USING target_list_opt_comma GROUP_P BY name_list_opt_comma_opt_bracket
        11 => {
            let mut res = SelectStmt { ..Default::default() };
            let n = PivotStmt {
                source: rhs.node(2)?,
                aggrs: rhs.list(4)?,
                groups: rhs.list(7)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // pivot_keyword table_ref GROUP_P BY name_list_opt_comma_opt_bracket
        12 => {
            let mut res = SelectStmt { ..Default::default() };
            let n = PivotStmt {
                source: rhs.node(2)?,
                groups: rhs.list(5)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // pivot_keyword table_ref ON pivot_column_list
        13 => {
            let mut res = SelectStmt { ..Default::default() };
            let n = PivotStmt {
                source: rhs.node(2)?,
                columns: rhs.list(4)?,
                ..Default::default()
            };
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // pivot_keyword table_ref ON pivot_column_list GROUP_P BY name_list_opt_comma_opt_bracket
        14 => {
            let mut res = SelectStmt { ..Default::default() };
            let n = PivotStmt {
                source: rhs.node(2)?,
                columns: rhs.list(4)?,
                groups: rhs.list(7)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // pivot_keyword table_ref ON pivot_column_list USING target_list_opt_comma
        15 => {
            let mut res = SelectStmt { ..Default::default() };
            let n = PivotStmt {
                source: rhs.node(2)?,
                columns: rhs.list(4)?,
                aggrs: rhs.list(6)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // pivot_keyword table_ref ON pivot_column_list USING target_list_opt_comma GROUP_P BY name_list_opt_comma_opt_bracket
        16 => {
            let mut res = SelectStmt { ..Default::default() };
            let n = PivotStmt {
                source: rhs.node(2)?,
                columns: rhs.list(4)?,
                aggrs: rhs.list(6)?,
                groups: rhs.list(9)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // unpivot_keyword table_ref ON target_list_opt_comma INTO NAME_P name value_or_values name_list_opt_comma_opt_bracket
        17 => {
            let mut res = SelectStmt { ..Default::default() };
            let mut n = PivotStmt {
                source: rhs.node(2)?,
                unpivots: rhs.list(9)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            let piv = Pivot {
                unpivot_columns: vec![Node::string(rhs.string(7)?)],
                pivot_value: rhs.list(4)?,
                ..Default::default()
            };
            n.columns = vec![piv.into()];
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        // unpivot_keyword table_ref ON target_list_opt_comma
        18 => {
            let mut res = SelectStmt { ..Default::default() };
            let mut n = PivotStmt {
                source: rhs.node(2)?,
                unpivots: vec![Node::string("value")],
                location: rhs.loc(1),
                ..Default::default()
            };
            let piv = Pivot {
                unpivot_columns: vec![Node::string("name")],
                pivot_value: rhs.list(4)?,
                ..Default::default()
            };
            n.columns = vec![piv.into()];
            res.pivot = Some(Box::new(n));
            Ok(SemValue::node(res))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// pivot_column_entry:
///       b_expr
///     | b_expr IN_P '(' select_no_parens ')'
///     | single_pivot_value
/// ```
pub(super) fn pivot_column_entry<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // b_expr
        0 => {
            let n = Pivot {
                pivot_columns: vec![rhs.required_node(1)?],
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // b_expr IN_P '(' select_no_parens ')'
        1 => {
            let n = Pivot {
                pivot_columns: vec![rhs.required_node(1)?],
                subquery: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// pivot_column_list_internal:
///       pivot_column_entry
///     | pivot_column_list_internal ',' pivot_column_entry
/// ```
pub(super) fn pivot_column_list_internal<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // pivot_column_entry
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // pivot_column_list_internal ',' pivot_column_entry
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// with_clause:
///       WITH cte_list
///     | WITH_LA cte_list
///     | WITH RECURSIVE cte_list
/// ```
pub(super) fn with_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH cte_list
        // WITH_LA cte_list
        0 | 1 => {
            let with_clause = WithClause {
                ctes: rhs.list(2)?,
                recursive: false,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(with_clause))
        }
        // WITH RECURSIVE cte_list
        2 => {
            let with_clause = WithClause {
                ctes: rhs.list(3)?,
                recursive: true,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(with_clause))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// cte_list:
///       common_table_expr
///     | cte_list ',' common_table_expr
/// ```
pub(super) fn cte_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // common_table_expr
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // cte_list ',' common_table_expr
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// common_table_expr:
///       name opt_name_list opt_on_key AS opt_materialized '(' PreparableStmt ')'
/// ```
pub(super) fn common_table_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // name opt_name_list opt_on_key AS opt_materialized '(' PreparableStmt ')'
        0 => {
            let n = CommonTableExpr {
                ctename: rhs.str(1)?,
                aliascolnames: rhs.list(2)?,
                recursive_keys: rhs.list(3)?,
                ctematerialized: rhs.cte_materialize(5)?,
                ctequery: rhs.node(7)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_on_key:
///       USING KEY '(' column_ref_list_opt_comma ')'
///     | /* empty */
/// ```
pub(super) fn opt_on_key<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// column_ref_list:
///       columnref
///     | column_ref_list ',' columnref
/// ```
pub(super) fn column_ref_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // columnref
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // column_ref_list ',' columnref
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_materialized:
///       MATERIALIZED
///     | NOT MATERIALIZED
///     | /* empty */
/// ```
pub(super) fn opt_materialized<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // MATERIALIZED
        0 => Ok(SemValue::CteMaterialize(CteMaterialize::Always)),
        // NOT MATERIALIZED
        1 => Ok(SemValue::CteMaterialize(CteMaterialize::Never)),
        // /* empty */
        2 => Ok(SemValue::CteMaterialize(CteMaterialize::Default)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// into_clause:
///       INTO OptTempTableName
///     | /* empty */
/// ```
pub(super) fn into_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // INTO OptTempTableName
        0 => {
            let into_clause = IntoClause {
                rel: rhs.ptr::<RangeVar>(2)?,
                col_names: List::new(),
                options: List::new(),
                on_commit: OnCommitAction::Noop,
                skip_data: false,
                ..Default::default()
            };
            Ok(SemValue::node(into_clause))
        }
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// OptTempTableName:
///       TEMPORARY opt_table qualified_name
///     | TEMP opt_table qualified_name
///     | LOCAL TEMPORARY opt_table qualified_name
///     | LOCAL TEMP opt_table qualified_name
///     | GLOBAL TEMPORARY opt_table qualified_name
///     | GLOBAL TEMP opt_table qualified_name
///     | UNLOGGED opt_table qualified_name
///     | TABLE qualified_name
///     | qualified_name
/// ```
pub(super) fn opt_temp_table_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TEMPORARY opt_table qualified_name
        // TEMP opt_table qualified_name
        0 | 1 => {
            let mut opt_temp_table_name = rhs.ptr::<RangeVar>(3)?;
            required_mut(&mut opt_temp_table_name)?.relpersistence = RelPersistence::Temp;
            Ok(SemValue::Node(opt_temp_table_name.map(Node::from)))
        }
        // LOCAL TEMPORARY opt_table qualified_name
        // LOCAL TEMP opt_table qualified_name
        2 | 3 => {
            let mut opt_temp_table_name = rhs.ptr::<RangeVar>(4)?;
            required_mut(&mut opt_temp_table_name)?.relpersistence = RelPersistence::Temp;
            Ok(SemValue::Node(opt_temp_table_name.map(Node::from)))
        }
        // GLOBAL TEMPORARY opt_table qualified_name
        // GLOBAL TEMP opt_table qualified_name
        4 | 5 => Err(rhs.error_at("GLOBAL is deprecated in temporary table creation", rhs.loc(1))),
        // UNLOGGED opt_table qualified_name
        6 => {
            let mut opt_temp_table_name = rhs.ptr::<RangeVar>(3)?;
            required_mut(&mut opt_temp_table_name)?.relpersistence = RelPersistence::Unlogged;
            Ok(SemValue::Node(opt_temp_table_name.map(Node::from)))
        }
        // TABLE qualified_name
        7 => {
            let mut opt_temp_table_name = rhs.ptr::<RangeVar>(2)?;
            required_mut(&mut opt_temp_table_name)?.relpersistence = RelPersistence::Permanent;
            Ok(SemValue::Node(opt_temp_table_name.map(Node::from)))
        }
        // qualified_name
        8 => {
            let mut opt_temp_table_name = rhs.ptr::<RangeVar>(1)?;
            required_mut(&mut opt_temp_table_name)?.relpersistence = RelPersistence::Permanent;
            Ok(SemValue::Node(opt_temp_table_name.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// all_or_distinct:
///       ALL
///     | DISTINCT
///     | /* empty */
/// ```
pub(super) fn all_or_distinct<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALL
        0 => Ok(SemValue::Bool(true)),
        // DISTINCT
        // /* empty */
        1 | 2 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// distinct_clause:
///       DISTINCT
///     | DISTINCT ON '(' expr_list_opt_comma ')'
/// ```
pub(super) fn distinct_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // DISTINCT
        0 => Ok(SemValue::OptList(Some(List::new()))),
        // DISTINCT ON '(' expr_list_opt_comma ')'
        1 => Ok(SemValue::OptList(Some(rhs.list(4)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_all_clause:
///       ALL
///     | /* empty */
/// ```
pub(super) fn opt_all_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALL
        // /* empty */
        0 | 1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_ignore_nulls:
///       IGNORE_P NULLS_P
///     | RESPECT_P NULLS_P
///     | /* empty */
/// ```
pub(super) fn opt_ignore_nulls<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // IGNORE_P NULLS_P
        0 => Ok(SemValue::IgnoreNulls(IgnoreNulls::Ignore)),
        // RESPECT_P NULLS_P
        1 => Ok(SemValue::IgnoreNulls(IgnoreNulls::Respect)),
        // /* empty */
        2 => Ok(SemValue::IgnoreNulls(IgnoreNulls::Default)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_sort_clause:
///       sort_clause
///     | /* empty */
/// ```
pub(super) fn opt_sort_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// sort_clause:
///       ORDER BY sortby_list
///     | ORDER BY ALL opt_asc_desc opt_nulls_order
/// ```
pub(super) fn sort_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ORDER BY ALL opt_asc_desc opt_nulls_order
        1 => {
            let mut sort = SortBy { ..Default::default() };
            let star = AStar {
                columns: true,
                location: rhs.loc(3),
                ..Default::default()
            };
            sort.node = Some(star.into());
            sort.sortby_dir = rhs.sort_by_dir(4)?;
            sort.sortby_nulls = rhs.sort_by_nulls(5)?;
            sort.use_op = List::new();
            sort.location = -1;
            Ok(SemValue::List(vec![sort.into()]))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// sortby_list:
///       sortby
///     | sortby_list ',' sortby
/// ```
pub(super) fn sortby_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // sortby
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // sortby_list ',' sortby
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// sortby:
///       a_expr USING qual_all_Op opt_nulls_order
///     | a_expr opt_asc_desc opt_nulls_order
/// ```
pub(super) fn sortby<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr USING qual_all_Op opt_nulls_order
        0 => {
            let sortby = SortBy {
                node: rhs.node(1)?,
                sortby_dir: SortByDir::Using,
                sortby_nulls: rhs.sort_by_nulls(4)?,
                use_op: rhs.list(3)?,
                location: rhs.loc(3),
            };
            Ok(SemValue::node(sortby))
        }
        // a_expr opt_asc_desc opt_nulls_order
        1 => {
            let sortby = SortBy {
                node: rhs.node(1)?,
                sortby_dir: rhs.sort_by_dir(2)?,
                sortby_nulls: rhs.sort_by_nulls(3)?,
                use_op: List::new(),
                location: -1,
            };
            Ok(SemValue::node(sortby))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_asc_desc:
///       ASC_P
///     | DESC_P
///     | /* empty */
/// ```
pub(super) fn opt_asc_desc<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ASC_P
        0 => Ok(SemValue::SortByDir(SortByDir::Asc)),
        // DESC_P
        1 => Ok(SemValue::SortByDir(SortByDir::Desc)),
        // /* empty */
        2 => Ok(SemValue::SortByDir(SortByDir::Default)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_nulls_order:
///       NULLS_LA FIRST_P
///     | NULLS_LA LAST_P
///     | /* empty */
/// ```
pub(super) fn opt_nulls_order<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // NULLS_LA FIRST_P
        0 => Ok(SemValue::SortByNulls(SortByNulls::First)),
        // NULLS_LA LAST_P
        1 => Ok(SemValue::SortByNulls(SortByNulls::Last)),
        // /* empty */
        2 => Ok(SemValue::SortByNulls(SortByNulls::Default)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// select_limit:
///       limit_clause offset_clause
///     | offset_clause limit_clause
///     | limit_clause
///     | offset_clause
/// ```
pub(super) fn select_limit<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // limit_clause offset_clause
        0 => Ok(SemValue::SelectLimit(Box::new(SelectLimit {
            offset: rhs.node(2)?,
            count: rhs.node(1)?,
            offset_first: false,
        }))),
        // offset_clause limit_clause
        1 => Ok(SemValue::SelectLimit(Box::new(SelectLimit {
            offset: rhs.node(1)?,
            count: rhs.node(2)?,
            offset_first: true,
        }))),
        // limit_clause
        2 => Ok(SemValue::SelectLimit(Box::new(SelectLimit {
            offset: None,
            count: rhs.node(1)?,
            offset_first: true,
        }))),
        // offset_clause
        3 => Ok(SemValue::SelectLimit(Box::new(SelectLimit {
            offset: rhs.node(1)?,
            count: None,
            offset_first: true,
        }))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_select_limit:
///       select_limit
///     | /* empty */
/// ```
pub(super) fn opt_select_limit<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // select_limit
        0 => Ok(SemValue::SelectLimit(rhs.select_limit(1)?)),
        // /* empty */
        1 => Ok(SemValue::SelectLimit(Box::new(SelectLimit::none()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// limit_clause:
///       LIMIT select_limit_value
///     | LIMIT select_limit_value ',' select_offset_value
///     | FETCH first_or_next select_fetch_first_value row_or_rows ONLY
///     | FETCH first_or_next row_or_rows ONLY
/// ```
pub(super) fn limit_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // LIMIT select_limit_value ',' select_offset_value
        1 => Err(unimplemented("errhint")),
        // FETCH first_or_next row_or_rows ONLY
        3 => Ok(SemValue::node(make_int_const(1, -1))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// sample_value:
///       FCONST
///     | ICONST
///     | param_expr
/// ```
pub(super) fn sample_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FCONST
        0 => Ok(SemValue::node(make_float_const(rhs.string(1)?, rhs.loc(1)))),
        // ICONST
        1 => Ok(SemValue::node(make_int_const(rhs.int(1)?, rhs.loc(1)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// sample_count:
///       sample_value '%'
///     | sample_value PERCENT
///     | sample_value
///     | sample_value ROWS
/// ```
pub(super) fn sample_count<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // sample_value '%'
        // sample_value PERCENT
        0 | 1 => Ok(SemValue::node(make_sample_size(rhs.node(1)?, true))),
        // sample_value
        // sample_value ROWS
        2 | 3 => Ok(SemValue::node(make_sample_size(rhs.node(1)?, false))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// sample_clause:
///       USING SAMPLE tablesample_entry
///     | /* empty */
/// ```
pub(super) fn sample_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_sample_func:
///       ColId
///     | /* empty */
/// ```
pub(super) fn opt_sample_func<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Str(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// tablesample_entry:
///       opt_sample_func '(' sample_count ')' opt_repeatable_clause
///     | sample_count
///     | sample_count '(' ColId ')'
///     | sample_count '(' ColId ',' ICONST ')'
/// ```
pub(super) fn tablesample_entry<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_sample_func '(' sample_count ')' opt_repeatable_clause
        0 => Ok(SemValue::node(make_sample_options(
            rhs.node(3)?,
            rhs.str(1)?,
            Some(rhs.int(5)?),
            rhs.loc(1),
        ))),
        // sample_count
        1 => Ok(SemValue::node(make_sample_options(
            rhs.node(1)?,
            None,
            None,
            rhs.loc(1),
        ))),
        // sample_count '(' ColId ')'
        2 => Ok(SemValue::node(make_sample_options(
            rhs.node(1)?,
            rhs.str(3)?,
            None,
            rhs.loc(1),
        ))),
        // sample_count '(' ColId ',' ICONST ')'
        3 => Ok(SemValue::node(make_sample_options(
            rhs.node(1)?,
            rhs.str(3)?,
            Some(rhs.int(5)?),
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_tablesample_clause:
///       tablesample_clause
///     | /* empty */
/// ```
pub(super) fn opt_tablesample_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_repeatable_clause:
///       REPEATABLE '(' ICONST ')'
///     | /* empty */
/// ```
pub(super) fn opt_repeatable_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Int(-1)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// at_unit:
///       TIMESTAMP
///     | VERSION_P
/// ```
pub(super) fn at_unit<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TIMESTAMP
        0 => Ok(SemValue::Str(Some("TIMESTAMP".into()))),
        // VERSION_P
        1 => Ok(SemValue::Str(Some("VERSION".into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// at_specifier:
///       at_unit EQUALS_GREATER a_expr
/// ```
pub(super) fn at_specifier<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // at_unit EQUALS_GREATER a_expr
        0 => {
            let n = AtClause {
                unit: rhs.str(1)?,
                expr: rhs.node(3)?,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_at_clause:
///       AT '(' at_specifier ')'
///     | /* empty */
/// ```
pub(super) fn opt_at_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// select_limit_value:
///       a_expr
///     | ALL
///     | a_expr '%'
///     | FCONST PERCENT
///     | ICONST PERCENT
/// ```
pub(super) fn select_limit_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ALL
        1 => Ok(SemValue::node(make_null_a_const(rhs.loc(1)))),
        // a_expr '%'
        2 => Ok(SemValue::node(make_limit_percent(rhs.node(1)?))),
        // FCONST PERCENT
        3 => Ok(SemValue::node(make_limit_percent(Some(make_float_const(
            rhs.string(1)?,
            rhs.loc(1),
        ))))),
        // ICONST PERCENT
        4 => Ok(SemValue::node(make_limit_percent(Some(make_int_const(
            rhs.int(1)?,
            rhs.loc(1),
        ))))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// select_fetch_first_value:
///       c_expr
///     | '+' I_or_F_const
///     | '-' I_or_F_const
/// ```
pub(super) fn select_fetch_first_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '+' I_or_F_const
        1 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "+",
            None,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        // '-' I_or_F_const
        2 => Ok(SemValue::node(do_negate(rhs.node(2)?, rhs.loc(1)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// I_or_F_const:
///       Iconst
///     | FCONST
/// ```
pub(super) fn i_or_f_const<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Iconst
        0 => Ok(SemValue::node(make_int_const(rhs.int(1)?, rhs.loc(1)))),
        // FCONST
        1 => Ok(SemValue::node(make_float_const(rhs.string(1)?, rhs.loc(1)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// row_or_rows:
///       ROW
///     | ROWS
/// ```
pub(super) fn row_or_rows<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ROW
        // ROWS
        0 | 1 => Ok(SemValue::Int(0)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// first_or_next:
///       FIRST_P
///     | NEXT
/// ```
pub(super) fn first_or_next<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FIRST_P
        // NEXT
        0 | 1 => Ok(SemValue::Int(0)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// group_clause:
///       GROUP_P BY group_by_list_opt_comma
///     | GROUP_P BY ALL
///     | /* empty */
/// ```
pub(super) fn group_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // GROUP_P BY ALL
        1 => {
            let all = make_grouping_set(GroupingSetKind::All, List::new(), rhs.loc(3));
            Ok(SemValue::List(vec![all.into()]))
        }
        // /* empty */
        2 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// group_by_list:
///       group_by_item
///     | group_by_list ',' group_by_item
/// ```
pub(super) fn group_by_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // group_by_item
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // group_by_list ',' group_by_item
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// empty_grouping_set:
///       '(' ')'
/// ```
pub(super) fn empty_grouping_set<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' ')'
        0 => Ok(SemValue::node(make_grouping_set(
            GroupingSetKind::Empty,
            List::new(),
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// rollup_clause:
///       ROLLUP '(' expr_list_opt_comma ')'
/// ```
pub(super) fn rollup_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ROLLUP '(' expr_list_opt_comma ')'
        0 => Ok(SemValue::node(make_grouping_set(
            GroupingSetKind::Rollup,
            rhs.list(3)?,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// cube_clause:
///       CUBE '(' expr_list_opt_comma ')'
/// ```
pub(super) fn cube_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CUBE '(' expr_list_opt_comma ')'
        0 => Ok(SemValue::node(make_grouping_set(
            GroupingSetKind::Cube,
            rhs.list(3)?,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// grouping_sets_clause:
///       GROUPING SETS '(' group_by_list_opt_comma ')'
/// ```
pub(super) fn grouping_sets_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // GROUPING SETS '(' group_by_list_opt_comma ')'
        0 => Ok(SemValue::node(make_grouping_set(
            GroupingSetKind::Sets,
            rhs.list(4)?,
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// grouping_or_grouping_id:
///       GROUPING
///     | GROUPING_ID
/// ```
pub(super) fn grouping_or_grouping_id<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // GROUPING
        // GROUPING_ID
        0 | 1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// having_clause:
///       HAVING a_expr
///     | /* empty */
/// ```
pub(super) fn having_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qualify_clause:
///       QUALIFY a_expr
///     | /* empty */
/// ```
pub(super) fn qualify_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// for_locking_clause:
///       for_locking_items
///     | FOR READ_P ONLY
/// ```
pub(super) fn for_locking_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FOR READ_P ONLY
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_for_locking_clause:
///       for_locking_clause
///     | /* empty */
/// ```
pub(super) fn opt_for_locking_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// for_locking_items:
///       for_locking_item
///     | for_locking_items for_locking_item
/// ```
pub(super) fn for_locking_items<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // for_locking_item
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // for_locking_items for_locking_item
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// for_locking_item:
///       for_locking_strength locked_rels_list opt_nowait_or_skip
/// ```
pub(super) fn for_locking_item<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // for_locking_strength locked_rels_list opt_nowait_or_skip
        0 => {
            let n = LockingClause {
                locked_rels: rhs.list(2)?,
                strength: rhs.lock_clause_strength(1)?,
                wait_policy: rhs.lock_wait_policy(3)?,
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// for_locking_strength:
///       FOR UPDATE
///     | FOR NO KEY UPDATE
///     | FOR SHARE
///     | FOR KEY SHARE
/// ```
pub(super) fn for_locking_strength<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FOR UPDATE
        0 => Ok(SemValue::LockClauseStrength(LockClauseStrength::ForUpdate)),
        // FOR NO KEY UPDATE
        1 => Ok(SemValue::LockClauseStrength(LockClauseStrength::ForNoKeyUpdate)),
        // FOR SHARE
        2 => Ok(SemValue::LockClauseStrength(LockClauseStrength::ForShare)),
        // FOR KEY SHARE
        3 => Ok(SemValue::LockClauseStrength(LockClauseStrength::ForKeyShare)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// locked_rels_list:
///       OF qualified_name_list
///     | /* empty */
/// ```
pub(super) fn locked_rels_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_nowait_or_skip:
///       NOWAIT
///     | SKIP LOCKED
///     | /* empty */
/// ```
pub(super) fn opt_nowait_or_skip<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // NOWAIT
        0 => Ok(SemValue::LockWaitPolicy(LockWaitPolicy::Error)),
        // SKIP LOCKED
        1 => Ok(SemValue::LockWaitPolicy(LockWaitPolicy::Skip)),
        // /* empty */
        2 => Ok(SemValue::LockWaitPolicy(LockWaitPolicy::Block)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// values_clause:
///       VALUES '(' expr_list_opt_comma ')'
///     | values_clause ',' '(' expr_list_opt_comma ')'
/// ```
pub(super) fn values_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // VALUES '(' expr_list_opt_comma ')'
        0 => {
            let n = SelectStmt {
                values_lists: vec![Node::List(rhs.list(3)?)],
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // values_clause ',' '(' expr_list_opt_comma ')'
        1 => {
            let mut n = rhs.take::<SelectStmt>(1)?;
            n.values_lists = append(n.values_lists, Node::List(rhs.list(4)?));
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// from_clause:
///       FROM from_list_opt_comma
///     | /* empty */
/// ```
pub(super) fn from_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// from_list:
///       table_ref
///     | from_list ',' table_ref
/// ```
pub(super) fn from_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // table_ref
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // from_list ',' table_ref
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alias_prefix_colon_clause:
///       ColIdOrString SINGLE_COLON
/// ```
pub(super) fn alias_prefix_colon_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColIdOrString SINGLE_COLON
        0 => {
            let alias_prefix_colon_clause = Alias {
                aliasname: rhs.str(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(alias_prefix_colon_clause))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// table_ref:
///       relation_expr opt_alias_clause opt_at_clause opt_tablesample_clause
///     | alias_prefix_colon_clause relation_expr opt_at_clause opt_tablesample_clause
///     | func_table func_alias_clause opt_tablesample_clause
///     | alias_prefix_colon_clause func_table opt_tablesample_clause
///     | values_clause_opt_comma alias_clause opt_tablesample_clause
///     | LATERAL_P func_table func_alias_clause
///     | select_with_parens opt_alias_clause opt_tablesample_clause
///     | alias_prefix_colon_clause select_with_parens opt_tablesample_clause
///     | LATERAL_P select_with_parens opt_alias_clause
///     | joined_table
///     | '(' joined_table ')' alias_clause
///     | alias_prefix_colon_clause '(' joined_table ')'
///     | table_ref PIVOT '(' target_list_opt_comma FOR pivot_value_list opt_pivot_group_by ')' opt_alias_clause
///     | table_ref UNPIVOT opt_include_nulls '(' unpivot_header FOR unpivot_value_list ')' opt_alias_clause
/// ```
pub(super) fn table_ref<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // relation_expr opt_alias_clause opt_at_clause opt_tablesample_clause
        0 => {
            let mut relation_expr = rhs.take::<RangeVar>(1)?;
            relation_expr.at_clause = rhs.node(3)?;
            relation_expr.alias = rhs.ptr::<Alias>(2)?;
            relation_expr.sample = rhs.node(4)?;
            Ok(SemValue::node(relation_expr))
        }
        // alias_prefix_colon_clause relation_expr opt_at_clause opt_tablesample_clause
        1 => {
            let mut relation_expr = rhs.take::<RangeVar>(2)?;
            relation_expr.at_clause = rhs.node(3)?;
            relation_expr.alias = rhs.ptr::<Alias>(1)?;
            relation_expr.sample = rhs.node(4)?;
            Ok(SemValue::node(relation_expr))
        }
        // func_table func_alias_clause opt_tablesample_clause
        2 => {
            let mut function = rhs.take::<RangeFunction>(1)?;
            let FuncAlias { alias, coldeflist } = *rhs.func_alias(2)?;
            function.alias = alias;
            function.coldeflist = coldeflist;
            function.sample = rhs.node(3)?;
            Ok(SemValue::node(function))
        }
        // alias_prefix_colon_clause func_table opt_tablesample_clause
        3 => {
            let mut n = rhs.take::<RangeFunction>(2)?;
            n.alias = rhs.ptr::<Alias>(1)?;
            n.sample = rhs.node(3)?;
            Ok(SemValue::node(n))
        }
        // values_clause_opt_comma alias_clause opt_tablesample_clause
        // select_with_parens opt_alias_clause opt_tablesample_clause
        4 | 6 => {
            let n = RangeSubselect {
                lateral: false,
                subquery: rhs.node(1)?,
                alias: rhs.ptr::<Alias>(2)?,
                sample: rhs.node(3)?,
            };
            Ok(SemValue::node(n))
        }
        // LATERAL_P func_table func_alias_clause
        5 => {
            let mut function = rhs.take::<RangeFunction>(2)?;
            let FuncAlias { alias, coldeflist } = *rhs.func_alias(3)?;
            function.lateral = true;
            function.alias = alias;
            function.coldeflist = coldeflist;
            Ok(SemValue::node(function))
        }
        // alias_prefix_colon_clause select_with_parens opt_tablesample_clause
        7 => {
            let n = RangeSubselect {
                lateral: false,
                subquery: rhs.node(2)?,
                alias: rhs.ptr::<Alias>(1)?,
                sample: rhs.node(3)?,
            };
            Ok(SemValue::node(n))
        }
        // LATERAL_P select_with_parens opt_alias_clause
        8 => {
            let n = RangeSubselect {
                lateral: true,
                subquery: rhs.node(2)?,
                alias: rhs.ptr::<Alias>(3)?,
                sample: None,
            };
            Ok(SemValue::node(n))
        }
        // '(' joined_table ')' alias_clause
        10 => {
            let mut joined_table = rhs.take::<JoinExpr>(2)?;
            joined_table.alias = rhs.ptr::<Alias>(4)?;
            Ok(SemValue::node(joined_table))
        }
        // alias_prefix_colon_clause '(' joined_table ')'
        11 => {
            let mut joined_table = rhs.take::<JoinExpr>(3)?;
            joined_table.alias = rhs.ptr::<Alias>(1)?;
            Ok(SemValue::node(joined_table))
        }
        // table_ref PIVOT '(' target_list_opt_comma FOR pivot_value_list opt_pivot_group_by ')' opt_alias_clause
        12 => {
            let n = PivotExpr {
                source: rhs.node(1)?,
                aggrs: rhs.list(4)?,
                pivots: rhs.list(6)?,
                groups: rhs.list(7)?,
                alias: rhs.ptr::<Alias>(9)?,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // table_ref UNPIVOT opt_include_nulls '(' unpivot_header FOR unpivot_value_list ')' opt_alias_clause
        13 => {
            let n = PivotExpr {
                source: rhs.node(1)?,
                include_nulls: rhs.bool(3)?,
                unpivots: rhs.list(5)?,
                pivots: rhs.list(7)?,
                alias: rhs.ptr::<Alias>(9)?,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_pivot_group_by:
///       GROUP_P BY name_list_opt_comma
///     | /* empty */
/// ```
pub(super) fn opt_pivot_group_by<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_include_nulls:
///       INCLUDE_P NULLS_P
///     | EXCLUDE NULLS_P
///     | /* empty */
/// ```
pub(super) fn opt_include_nulls<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // INCLUDE_P NULLS_P
        0 => Ok(SemValue::Bool(true)),
        // EXCLUDE NULLS_P
        // /* empty */
        1 | 2 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// single_pivot_value:
///       b_expr IN_P '(' target_list_opt_comma ')'
///     | b_expr IN_P ColIdOrString
/// ```
pub(super) fn single_pivot_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // b_expr IN_P '(' target_list_opt_comma ')'
        0 => {
            let n = Pivot {
                pivot_columns: vec![rhs.required_node(1)?],
                pivot_value: rhs.list(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // b_expr IN_P ColIdOrString
        1 => {
            let n = Pivot {
                pivot_columns: vec![rhs.required_node(1)?],
                pivot_enum: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// pivot_header:
///       d_expr
///     | indirection_expr
///     | '(' c_expr_list_opt_comma ')'
/// ```
pub(super) fn pivot_header<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // d_expr
        // indirection_expr
        0 | 1 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// pivot_value:
///       pivot_header IN_P '(' target_list_opt_comma ')'
///     | pivot_header IN_P ColIdOrString
/// ```
pub(super) fn pivot_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // pivot_header IN_P '(' target_list_opt_comma ')'
        0 => {
            let n = Pivot {
                pivot_columns: rhs.list(1)?,
                pivot_value: rhs.list(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // pivot_header IN_P ColIdOrString
        1 => {
            let n = Pivot {
                pivot_columns: rhs.list(1)?,
                pivot_enum: rhs.str(3)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// pivot_value_list:
///       pivot_value
///     | pivot_value_list pivot_value
/// ```
pub(super) fn pivot_value_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // pivot_value
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // pivot_value_list pivot_value
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// unpivot_header:
///       ColIdOrString
///     | '(' name_list_opt_comma ')'
/// ```
pub(super) fn unpivot_header<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColIdOrString
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// unpivot_value:
///       unpivot_header IN_P '(' target_list_opt_comma ')'
/// ```
pub(super) fn unpivot_value<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // unpivot_header IN_P '(' target_list_opt_comma ')'
        0 => {
            let n = Pivot {
                unpivot_columns: rhs.list(1)?,
                pivot_value: rhs.list(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// unpivot_value_list:
///       unpivot_value
///     | unpivot_value_list unpivot_value
/// ```
pub(super) fn unpivot_value_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // unpivot_value
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // unpivot_value_list unpivot_value
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// joined_table:
///       '(' joined_table ')'
///     | table_ref CROSS JOIN table_ref
///     | table_ref join_type JOIN table_ref join_qual
///     | table_ref JOIN table_ref join_qual
///     | table_ref NATURAL join_type JOIN table_ref
///     | table_ref NATURAL JOIN table_ref
///     | table_ref ASOF join_type JOIN table_ref join_qual
///     | table_ref ASOF JOIN table_ref join_qual
///     | table_ref POSITIONAL JOIN table_ref
///     | table_ref ANTI JOIN table_ref join_qual
///     | table_ref SEMI JOIN table_ref join_qual
/// ```
pub(super) fn joined_table<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // table_ref CROSS JOIN table_ref
        1 => {
            let n = JoinExpr {
                jointype: JoinType::Inner,
                joinreftype: JoinRefType::Regular,
                larg: rhs.node(1)?,
                rarg: rhs.node(4)?,
                using_clause: List::new(),
                quals: None,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // table_ref join_type JOIN table_ref join_qual
        2 => {
            let join_qual = rhs.node(5)?;
            let mut n = JoinExpr {
                jointype: rhs.join_type(2)?,
                joinreftype: JoinRefType::Regular,
                larg: rhs.node(1)?,
                rarg: rhs.node(4)?,
                ..Default::default()
            };
            if join_qual.is_some() && matches!(join_qual, Some(Node::List(_))) {
                n.using_clause = into_list(join_qual)?;
            } else {
                n.quals = join_qual;
            }
            n.location = rhs.loc(2);
            Ok(SemValue::node(n))
        }
        // table_ref JOIN table_ref join_qual
        3 => {
            let join_qual = rhs.node(4)?;
            let mut n = JoinExpr {
                jointype: JoinType::Inner,
                joinreftype: JoinRefType::Regular,
                larg: rhs.node(1)?,
                rarg: rhs.node(3)?,
                ..Default::default()
            };
            if join_qual.is_some() && matches!(join_qual, Some(Node::List(_))) {
                n.using_clause = into_list(join_qual)?;
            } else {
                n.quals = join_qual;
            }
            n.location = rhs.loc(2);
            Ok(SemValue::node(n))
        }
        // table_ref NATURAL join_type JOIN table_ref
        4 => {
            let n = JoinExpr {
                jointype: rhs.join_type(3)?,
                joinreftype: JoinRefType::Natural,
                larg: rhs.node(1)?,
                rarg: rhs.node(5)?,
                using_clause: List::new(),
                quals: None,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // table_ref NATURAL JOIN table_ref
        5 => {
            let n = JoinExpr {
                jointype: JoinType::Inner,
                joinreftype: JoinRefType::Natural,
                larg: rhs.node(1)?,
                rarg: rhs.node(4)?,
                using_clause: List::new(),
                quals: None,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // table_ref ASOF join_type JOIN table_ref join_qual
        6 => {
            let join_qual = rhs.node(6)?;
            let mut n = JoinExpr {
                jointype: rhs.join_type(3)?,
                joinreftype: JoinRefType::Asof,
                larg: rhs.node(1)?,
                rarg: rhs.node(5)?,
                ..Default::default()
            };
            if join_qual.is_some() && matches!(join_qual, Some(Node::List(_))) {
                n.using_clause = into_list(join_qual)?;
            } else {
                n.quals = join_qual;
            }
            n.location = rhs.loc(2);
            Ok(SemValue::node(n))
        }
        // table_ref ASOF JOIN table_ref join_qual
        7 => {
            let join_qual = rhs.node(5)?;
            let mut n = JoinExpr {
                jointype: JoinType::Inner,
                joinreftype: JoinRefType::Asof,
                larg: rhs.node(1)?,
                rarg: rhs.node(4)?,
                ..Default::default()
            };
            if join_qual.is_some() && matches!(join_qual, Some(Node::List(_))) {
                n.using_clause = into_list(join_qual)?;
            } else {
                n.quals = join_qual;
            }
            n.location = rhs.loc(2);
            Ok(SemValue::node(n))
        }
        // table_ref POSITIONAL JOIN table_ref
        8 => {
            let n = JoinExpr {
                jointype: JoinType::Position,
                joinreftype: JoinRefType::Regular,
                larg: rhs.node(1)?,
                rarg: rhs.node(4)?,
                using_clause: List::new(),
                quals: None,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // table_ref ANTI JOIN table_ref join_qual
        9 => {
            let join_qual = rhs.node(5)?;
            let mut n = JoinExpr {
                jointype: JoinType::Anti,
                joinreftype: JoinRefType::Regular,
                larg: rhs.node(1)?,
                rarg: rhs.node(4)?,
                ..Default::default()
            };
            if join_qual.is_some() && matches!(join_qual, Some(Node::List(_))) {
                n.using_clause = into_list(join_qual)?;
            } else {
                n.quals = join_qual;
            }
            n.location = rhs.loc(2);
            Ok(SemValue::node(n))
        }
        // table_ref SEMI JOIN table_ref join_qual
        10 => {
            let join_qual = rhs.node(5)?;
            let mut n = JoinExpr {
                jointype: JoinType::Semi,
                joinreftype: JoinRefType::Regular,
                larg: rhs.node(1)?,
                rarg: rhs.node(4)?,
                ..Default::default()
            };
            if join_qual.is_some() && matches!(join_qual, Some(Node::List(_))) {
                n.using_clause = into_list(join_qual)?;
            } else {
                n.quals = join_qual;
            }
            n.location = rhs.loc(2);
            n.location = rhs.loc(2);
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// alias_clause:
///       AS ColIdOrString '(' name_list_opt_comma ')'
///     | AS ColIdOrString
///     | ColId '(' name_list_opt_comma ')'
///     | ColId
/// ```
pub(super) fn alias_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // AS ColIdOrString '(' name_list_opt_comma ')'
        0 => {
            let alias_clause = Alias {
                aliasname: rhs.str(2)?,
                colnames: rhs.list(4)?,
            };
            Ok(SemValue::node(alias_clause))
        }
        // AS ColIdOrString
        1 => {
            let alias_clause = Alias {
                aliasname: rhs.str(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(alias_clause))
        }
        // ColId '(' name_list_opt_comma ')'
        2 => {
            let alias_clause = Alias {
                aliasname: rhs.str(1)?,
                colnames: rhs.list(3)?,
            };
            Ok(SemValue::node(alias_clause))
        }
        // ColId
        3 => {
            let alias_clause = Alias {
                aliasname: rhs.str(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(alias_clause))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_alias_clause:
///       alias_clause
///     | /* empty */
/// ```
pub(super) fn opt_alias_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_alias_clause:
///       alias_clause
///     | AS '(' TableFuncElementList ')'
///     | AS ColIdOrString '(' TableFuncElementList ')'
///     | ColId '(' TableFuncElementList ')'
///     | /* empty */
/// ```
pub(super) fn func_alias_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // alias_clause
        0 => Ok(SemValue::FuncAlias(Box::new(FuncAlias {
            alias: rhs.ptr::<Alias>(1)?,
            coldeflist: List::new(),
        }))),
        // AS '(' TableFuncElementList ')'
        1 => Ok(SemValue::FuncAlias(Box::new(FuncAlias {
            alias: None,
            coldeflist: rhs.list(3)?,
        }))),
        // AS ColIdOrString '(' TableFuncElementList ')'
        2 => {
            let alias = Alias {
                aliasname: rhs.str(2)?,
                ..Default::default()
            };
            Ok(SemValue::FuncAlias(Box::new(FuncAlias {
                alias: Some(Box::new(alias)),
                coldeflist: rhs.list(4)?,
            })))
        }
        // ColId '(' TableFuncElementList ')'
        3 => {
            let alias = Alias {
                aliasname: rhs.str(1)?,
                ..Default::default()
            };
            Ok(SemValue::FuncAlias(Box::new(FuncAlias {
                alias: Some(Box::new(alias)),
                coldeflist: rhs.list(3)?,
            })))
        }
        // /* empty */
        4 => Ok(SemValue::FuncAlias(Box::default())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// join_type:
///       FULL join_outer
///     | LEFT join_outer
///     | RIGHT join_outer
///     | SEMI
///     | ANTI
///     | INNER_P
/// ```
pub(super) fn join_type<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FULL join_outer
        0 => Ok(SemValue::JoinType(JoinType::Full)),
        // LEFT join_outer
        1 => Ok(SemValue::JoinType(JoinType::Left)),
        // RIGHT join_outer
        2 => Ok(SemValue::JoinType(JoinType::Right)),
        // SEMI
        3 => Ok(SemValue::JoinType(JoinType::Semi)),
        // ANTI
        4 => Ok(SemValue::JoinType(JoinType::Anti)),
        // INNER_P
        5 => Ok(SemValue::JoinType(JoinType::Inner)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// join_outer:
///       OUTER_P
///     | /* empty */
/// ```
pub(super) fn join_outer<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // OUTER_P
        // /* empty */
        0 | 1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// join_qual:
///       USING '(' name_list_opt_comma ')'
///     | ON a_expr
/// ```
pub(super) fn join_qual<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // USING '(' name_list_opt_comma ')'
        0 => Ok(SemValue::Node(Node::from_list(rhs.list(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// relation_expr:
///       qualified_name
///     | qualified_name '*'
///     | ONLY qualified_name
///     | ONLY '(' qualified_name ')'
/// ```
pub(super) fn relation_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // qualified_name
        // qualified_name '*'
        0 | 1 => {
            let mut relation_expr = rhs.ptr::<RangeVar>(1)?;
            required_mut(&mut relation_expr)?.inh = true;
            required_mut(&mut relation_expr)?.alias = None;
            Ok(SemValue::Node(relation_expr.map(Node::from)))
        }
        // ONLY qualified_name
        2 => {
            let mut relation_expr = rhs.ptr::<RangeVar>(2)?;
            required_mut(&mut relation_expr)?.inh = false;
            required_mut(&mut relation_expr)?.alias = None;
            Ok(SemValue::Node(relation_expr.map(Node::from)))
        }
        // ONLY '(' qualified_name ')'
        3 => {
            let mut relation_expr = rhs.ptr::<RangeVar>(3)?;
            required_mut(&mut relation_expr)?.inh = false;
            required_mut(&mut relation_expr)?.alias = None;
            Ok(SemValue::Node(relation_expr.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_table:
///       func_expr_windowless opt_ordinality
///     | ROWS FROM '(' rowsfrom_list ')' opt_ordinality
/// ```
pub(super) fn func_table<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // func_expr_windowless opt_ordinality
        0 => {
            // each function is paired with its (here empty) column definition list
            let function = vec![rhs.required_node(1)?, Node::List(List::new())];
            Ok(SemValue::node(RangeFunction {
                lateral: false,
                ordinality: rhs.bool(2)?,
                is_rowsfrom: false,
                functions: vec![Node::List(function)],
                sample: None,
                ..Default::default()
            }))
        }
        // ROWS FROM '(' rowsfrom_list ')' opt_ordinality
        1 => {
            let n = RangeFunction {
                lateral: false,
                ordinality: rhs.bool(6)?,
                is_rowsfrom: true,
                functions: rhs.list(4)?,
                sample: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// rowsfrom_item:
///       func_expr_windowless opt_col_def_list
/// ```
pub(super) fn rowsfrom_item<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // func_expr_windowless opt_col_def_list
        0 => Ok(SemValue::List(vec![
            rhs.required_node(1)?,
            Node::List(rhs.list(2)?),
        ])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// rowsfrom_list:
///       rowsfrom_item
///     | rowsfrom_list ',' rowsfrom_item
/// ```
pub(super) fn rowsfrom_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // rowsfrom_item
        0 => Ok(SemValue::List(vec![Node::List(rhs.list(1)?)])),
        // rowsfrom_list ',' rowsfrom_item
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::List(rhs.list(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_col_def_list:
///       AS '(' TableFuncElementList ')'
///     | /* empty */
/// ```
pub(super) fn opt_col_def_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_ordinality:
///       WITH_LA ORDINALITY
///     | /* empty */
/// ```
pub(super) fn opt_ordinality<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH_LA ORDINALITY
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// where_clause:
///       WHERE a_expr
///     | /* empty */
/// ```
pub(super) fn where_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableFuncElementList:
///       TableFuncElement
///     | TableFuncElementList ',' TableFuncElement
/// ```
pub(super) fn table_func_element_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TableFuncElement
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // TableFuncElementList ',' TableFuncElement
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// TableFuncElement:
///       ColIdOrString Typename opt_collate_clause
/// ```
pub(super) fn table_func_element<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColIdOrString Typename opt_collate_clause
        0 => {
            let n = ColumnDef {
                colname: rhs.str(1)?,
                type_name: rhs.ptr::<TypeName>(2)?,
                raw_default: None,
                coll_clause: downcast_opt::<CollateClause>(rhs.node(3)?)?,
                constraints: List::new(),
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_collate_clause:
///       COLLATE any_name
///     | /* empty */
/// ```
pub(super) fn opt_collate_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // COLLATE any_name
        0 => {
            let n = CollateClause {
                arg: None,
                collname: rhs.list(2)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(n))
        }
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// colid_type_list:
///       ColId Typename
///     | colid_type_list ',' ColId Typename
/// ```
pub(super) fn colid_type_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId Typename
        0 => Ok(SemValue::List(vec![Node::List(vec![
            Node::string(rhs.string(1)?),
            rhs.required_node(2)?,
        ])])),
        // colid_type_list ',' ColId Typename
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::List(vec![Node::string(rhs.string(3)?), rhs.required_node(4)?]),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_Typename:
///       Typename
///     | /* empty */
/// ```
pub(super) fn opt_typename<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// Typename:
///       SimpleTypename opt_array_bounds
///     | qualified_typename opt_array_bounds
///     | SETOF SimpleTypename opt_array_bounds
///     | SimpleTypename ARRAY '[' Iconst ']'
///     | qualified_typename ARRAY '[' Iconst ']'
///     | SETOF SimpleTypename ARRAY '[' Iconst ']'
///     | SETOF qualified_typename ARRAY '[' Iconst ']'
///     | SimpleTypename ARRAY
///     | qualified_typename ARRAY
///     | SETOF SimpleTypename ARRAY
///     | SETOF qualified_typename ARRAY
///     | RowOrStruct '(' colid_type_list ')' opt_array_bounds
///     | MAP '(' type_list ')' opt_array_bounds
///     | UNION '(' colid_type_list ')' opt_array_bounds
/// ```
pub(super) fn typename<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // SimpleTypename opt_array_bounds
        0 => {
            let mut typename = rhs.ptr::<TypeName>(1)?;
            required_mut(&mut typename)?.array_bounds = rhs.list(2)?;
            Ok(SemValue::Node(typename.map(Node::from)))
        }
        // qualified_typename opt_array_bounds
        1 => {
            let mut typename = make_type_name_from_name_list(rhs.list(1)?);
            typename.array_bounds = rhs.list(2)?;
            Ok(SemValue::node(typename))
        }
        // SETOF SimpleTypename opt_array_bounds
        2 => {
            let mut typename = rhs.ptr::<TypeName>(2)?;
            required_mut(&mut typename)?.array_bounds = rhs.list(3)?;
            required_mut(&mut typename)?.setof = true;
            Ok(SemValue::Node(typename.map(Node::from)))
        }
        // SimpleTypename ARRAY '[' Iconst ']'
        3 => {
            let mut typename = rhs.ptr::<TypeName>(1)?;
            required_mut(&mut typename)?.array_bounds =
                vec![Node::Value(Value::Integer(i64::from(rhs.int(4)?)))];
            Ok(SemValue::Node(typename.map(Node::from)))
        }
        // qualified_typename ARRAY '[' Iconst ']'
        4 => {
            let mut typename = make_type_name_from_name_list(rhs.list(1)?);
            typename.array_bounds = vec![Node::Value(Value::Integer(i64::from(rhs.int(4)?)))];
            Ok(SemValue::node(typename))
        }
        // SETOF SimpleTypename ARRAY '[' Iconst ']'
        5 => {
            let mut typename = rhs.ptr::<TypeName>(2)?;
            required_mut(&mut typename)?.array_bounds =
                vec![Node::Value(Value::Integer(i64::from(rhs.int(5)?)))];
            required_mut(&mut typename)?.setof = true;
            Ok(SemValue::Node(typename.map(Node::from)))
        }
        // SETOF qualified_typename ARRAY '[' Iconst ']'
        6 => {
            let mut typename = make_type_name_from_name_list(rhs.list(2)?);
            typename.array_bounds = vec![Node::Value(Value::Integer(i64::from(rhs.int(5)?)))];
            typename.setof = true;
            Ok(SemValue::node(typename))
        }
        // SimpleTypename ARRAY
        7 => {
            let mut typename = rhs.ptr::<TypeName>(1)?;
            required_mut(&mut typename)?.array_bounds = vec![Node::Value(Value::Integer(i64::from(-1)))];
            Ok(SemValue::Node(typename.map(Node::from)))
        }
        // qualified_typename ARRAY
        8 => {
            let mut typename = make_type_name_from_name_list(rhs.list(1)?);
            typename.array_bounds = vec![Node::Value(Value::Integer(i64::from(-1)))];
            Ok(SemValue::node(typename))
        }
        // SETOF SimpleTypename ARRAY
        9 => {
            let mut typename = rhs.ptr::<TypeName>(2)?;
            required_mut(&mut typename)?.array_bounds = vec![Node::Value(Value::Integer(i64::from(-1)))];
            required_mut(&mut typename)?.setof = true;
            Ok(SemValue::Node(typename.map(Node::from)))
        }
        // SETOF qualified_typename ARRAY
        10 => {
            let mut typename = make_type_name_from_name_list(rhs.list(2)?);
            typename.array_bounds = vec![Node::Value(Value::Integer(i64::from(-1)))];
            typename.setof = true;
            Ok(SemValue::node(typename))
        }
        // RowOrStruct '(' colid_type_list ')' opt_array_bounds
        11 => {
            let mut typename = system_type_name("struct");
            typename.array_bounds = rhs.list(5)?;
            typename.typmods = rhs.list(3)?;
            typename.location = rhs.loc(1);
            Ok(SemValue::node(typename))
        }
        // MAP '(' type_list ')' opt_array_bounds
        12 => {
            let mut typename = system_type_name("map");
            typename.array_bounds = rhs.list(5)?;
            typename.typmods = rhs.list(3)?;
            typename.location = rhs.loc(1);
            Ok(SemValue::node(typename))
        }
        // UNION '(' colid_type_list ')' opt_array_bounds
        13 => {
            let mut typename = system_type_name("union");
            typename.array_bounds = rhs.list(5)?;
            typename.typmods = rhs.list(3)?;
            typename.location = rhs.loc(1);
            Ok(SemValue::node(typename))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qualified_typename:
///       IDENT '.' IDENT
///     | qualified_typename '.' IDENT
/// ```
pub(super) fn qualified_typename<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // IDENT '.' IDENT
        0 => Ok(SemValue::List(vec![
            Node::string(rhs.string(1)?),
            Node::string(rhs.string(3)?),
        ])),
        // qualified_typename '.' IDENT
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::string(rhs.string(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_array_bounds:
///       opt_array_bounds '[' ']'
///     | opt_array_bounds '[' Iconst ']'
///     | /* empty */
/// ```
pub(super) fn opt_array_bounds<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_array_bounds '[' ']'
        0 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::Value(Value::Integer(i64::from(-1))),
        ))),
        // opt_array_bounds '[' Iconst ']'
        1 => Ok(SemValue::List(append(
            rhs.list(1)?,
            Node::Value(Value::Integer(i64::from(rhs.int(3)?))),
        ))),
        // /* empty */
        2 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// SimpleTypename:
///       GenericType
///     | Numeric
///     | Bit
///     | Character
///     | ConstDatetime
///     | ConstInterval opt_interval
///     | ConstInterval '(' Iconst ')'
/// ```
pub(super) fn simple_typename<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ConstInterval opt_interval
        5 => {
            let mut simple_typename = rhs.ptr::<TypeName>(1)?;
            required_mut(&mut simple_typename)?.typmods = rhs.list(2)?;
            Ok(SemValue::Node(simple_typename.map(Node::from)))
        }
        // ConstInterval '(' Iconst ')'
        6 => {
            let mut simple_typename = rhs.ptr::<TypeName>(1)?;
            required_mut(&mut simple_typename)?.typmods = vec![
                make_int_const(INTERVAL_FULL_RANGE, -1),
                make_int_const(rhs.int(3)?, rhs.loc(3)),
            ];
            Ok(SemValue::Node(simple_typename.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// GenericType:
///       type_name_token opt_type_modifiers
/// ```
pub(super) fn generic_type<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // type_name_token opt_type_modifiers
        0 => {
            let mut generic_type = make_type_name(rhs.string(1)?);
            generic_type.typmods = rhs.list(2)?;
            generic_type.location = rhs.loc(1);
            Ok(SemValue::node(generic_type))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_type_modifiers:
///       '(' opt_expr_list_opt_comma ')'
///     | /* empty */
/// ```
pub(super) fn opt_type_modifiers<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// Numeric:
///       INT_P
///     | INTEGER
///     | SMALLINT
///     | BIGINT
///     | REAL
///     | FLOAT_P opt_float
///     | DOUBLE_P PRECISION
///     | DECIMAL_P opt_type_modifiers
///     | DEC opt_type_modifiers
///     | NUMERIC opt_type_modifiers
///     | BOOLEAN_P
/// ```
pub(super) fn numeric<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // INT_P
        // INTEGER
        0 | 1 => {
            let mut numeric = system_type_name("int4");
            numeric.location = rhs.loc(1);
            Ok(SemValue::node(numeric))
        }
        // SMALLINT
        2 => {
            let mut numeric = system_type_name("int2");
            numeric.location = rhs.loc(1);
            Ok(SemValue::node(numeric))
        }
        // BIGINT
        3 => {
            let mut numeric = system_type_name("int8");
            numeric.location = rhs.loc(1);
            Ok(SemValue::node(numeric))
        }
        // REAL
        4 => {
            let mut numeric = system_type_name("float4");
            numeric.location = rhs.loc(1);
            Ok(SemValue::node(numeric))
        }
        // FLOAT_P opt_float
        5 => {
            let mut numeric = rhs.ptr::<TypeName>(2)?;
            required_mut(&mut numeric)?.location = rhs.loc(1);
            Ok(SemValue::Node(numeric.map(Node::from)))
        }
        // DOUBLE_P PRECISION
        6 => {
            let mut numeric = system_type_name("float8");
            numeric.location = rhs.loc(1);
            Ok(SemValue::node(numeric))
        }
        // DECIMAL_P opt_type_modifiers
        // DEC opt_type_modifiers
        // NUMERIC opt_type_modifiers
        7..=9 => {
            let mut numeric = system_type_name("numeric");
            numeric.typmods = rhs.list(2)?;
            numeric.location = rhs.loc(1);
            Ok(SemValue::node(numeric))
        }
        // BOOLEAN_P
        10 => {
            let mut numeric = system_type_name("bool");
            numeric.location = rhs.loc(1);
            Ok(SemValue::node(numeric))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_float:
///       '(' Iconst ')'
///     | /* empty */
/// ```
pub(super) fn opt_float<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' Iconst ')'
        0 => {
            // check FLOAT() precision limits assuming IEEE floating types
            let name = match rhs.int(2)? {
                ..1 => {
                    return Err(rhs.error_at("precision for type float must be at least 1 bit", rhs.loc(2)));
                }
                1..=24 => "float4",
                25..=53 => "float8",
                _ => {
                    return Err(
                        rhs.error_at("precision for type float must be less than 54 bits", rhs.loc(2))
                    );
                }
            };
            Ok(SemValue::node(system_type_name(name)))
        }
        // /* empty */
        1 => Ok(SemValue::node(system_type_name("float4"))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstBit:
///       BitWithLength
///     | BitWithoutLength
/// ```
pub(super) fn const_bit<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BitWithoutLength
        1 => {
            let mut const_bit = rhs.ptr::<TypeName>(1)?;
            required_mut(&mut const_bit)?.typmods = List::new();
            Ok(SemValue::Node(const_bit.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// BitWithLength:
///       BIT opt_varying '(' expr_list_opt_comma ')'
/// ```
pub(super) fn bit_with_length<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BIT opt_varying '(' expr_list_opt_comma ')'
        0 => {
            let name = if rhs.bool(2)? { "varbit" } else { "bit" };
            let mut type_name = system_type_name(name);
            type_name.typmods = rhs.list(4)?;
            type_name.location = rhs.loc(1);
            Ok(SemValue::node(type_name))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// BitWithoutLength:
///       BIT opt_varying
/// ```
pub(super) fn bit_without_length<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BIT opt_varying
        0 => {
            let mut bit_without_length: Box<TypeName>;
            if rhs.bool(2)? {
                bit_without_length = Box::new(system_type_name("varbit"));
            } else {
                bit_without_length = Box::new(system_type_name("bit"));
                bit_without_length.typmods = vec![make_int_const(1, -1)];
            }
            bit_without_length.location = rhs.loc(1);
            Ok(SemValue::node(bit_without_length))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstCharacter:
///       CharacterWithLength
///     | CharacterWithoutLength
/// ```
pub(super) fn const_character<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CharacterWithoutLength
        1 => {
            let mut const_character = rhs.ptr::<TypeName>(1)?;
            required_mut(&mut const_character)?.typmods = List::new();
            Ok(SemValue::Node(const_character.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// CharacterWithLength:
///       character '(' Iconst ')'
/// ```
pub(super) fn character_with_length<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // character '(' Iconst ')'
        0 => {
            let mut character_with_length = system_type_name(required(rhs.const_str(1)?)?);
            character_with_length.typmods = vec![make_int_const(rhs.int(3)?, rhs.loc(3))];
            character_with_length.location = rhs.loc(1);
            Ok(SemValue::node(character_with_length))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// CharacterWithoutLength:
///       character
/// ```
pub(super) fn character_without_length<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // character
        0 => {
            let character = rhs.const_str(1)?;
            let mut character_without_length = system_type_name(required(character)?);
            if character == Some("bpchar") {
                character_without_length.typmods = vec![make_int_const(1, -1)];
            }
            character_without_length.location = rhs.loc(1);
            Ok(SemValue::node(character_without_length))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// character:
///       CHARACTER opt_varying
///     | CHAR_P opt_varying
///     | VARCHAR
///     | NATIONAL CHARACTER opt_varying
///     | NATIONAL CHAR_P opt_varying
///     | NCHAR opt_varying
/// ```
pub(super) fn character<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CHARACTER opt_varying
        // CHAR_P opt_varying
        // NCHAR opt_varying
        0 | 1 | 5 => Ok(SemValue::ConstStr(if rhs.bool(2)? {
            Some("varchar")
        } else {
            Some("bpchar")
        })),
        // VARCHAR
        2 => Ok(SemValue::ConstStr(Some("varchar"))),
        // NATIONAL CHARACTER opt_varying
        // NATIONAL CHAR_P opt_varying
        3 | 4 => Ok(SemValue::ConstStr(if rhs.bool(3)? {
            Some("varchar")
        } else {
            Some("bpchar")
        })),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_varying:
///       VARYING
///     | /* empty */
/// ```
pub(super) fn opt_varying<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // VARYING
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstDatetime:
///       TIMESTAMP '(' Iconst ')' opt_timezone
///     | TIMESTAMP opt_timezone
///     | TIME '(' Iconst ')' opt_timezone
///     | TIME opt_timezone
/// ```
pub(super) fn const_datetime<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // TIMESTAMP '(' Iconst ')' opt_timezone
        0 => {
            let mut const_datetime: Box<TypeName>;
            if rhs.bool(5)? {
                const_datetime = Box::new(system_type_name("timestamptz"));
            } else {
                const_datetime = Box::new(system_type_name("timestamp"));
            }
            const_datetime.typmods = vec![make_int_const(rhs.int(3)?, rhs.loc(3))];
            const_datetime.location = rhs.loc(1);
            Ok(SemValue::node(const_datetime))
        }
        // TIMESTAMP opt_timezone
        1 => {
            let mut const_datetime: Box<TypeName>;
            if rhs.bool(2)? {
                const_datetime = Box::new(system_type_name("timestamptz"));
            } else {
                const_datetime = Box::new(system_type_name("timestamp"));
            }
            const_datetime.location = rhs.loc(1);
            Ok(SemValue::node(const_datetime))
        }
        // TIME '(' Iconst ')' opt_timezone
        2 => {
            let mut const_datetime: Box<TypeName>;
            if rhs.bool(5)? {
                const_datetime = Box::new(system_type_name("timetz"));
            } else {
                const_datetime = Box::new(system_type_name("time"));
            }
            const_datetime.typmods = vec![make_int_const(rhs.int(3)?, rhs.loc(3))];
            const_datetime.location = rhs.loc(1);
            Ok(SemValue::node(const_datetime))
        }
        // TIME opt_timezone
        3 => {
            let mut const_datetime: Box<TypeName>;
            if rhs.bool(2)? {
                const_datetime = Box::new(system_type_name("timetz"));
            } else {
                const_datetime = Box::new(system_type_name("time"));
            }
            const_datetime.location = rhs.loc(1);
            Ok(SemValue::node(const_datetime))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// ConstInterval:
///       INTERVAL
/// ```
pub(super) fn const_interval<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // INTERVAL
        0 => {
            let mut const_interval = system_type_name("interval");
            const_interval.location = rhs.loc(1);
            Ok(SemValue::node(const_interval))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_timezone:
///       WITH_LA TIME ZONE
///     | WITHOUT TIME ZONE
///     | /* empty */
/// ```
pub(super) fn opt_timezone<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH_LA TIME ZONE
        0 => Ok(SemValue::Bool(true)),
        // WITHOUT TIME ZONE
        // /* empty */
        1 | 2 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_interval:
///       year_keyword
///     | month_keyword
///     | day_keyword
///     | hour_keyword
///     | minute_keyword
///     | second_keyword
///     | millisecond_keyword
///     | microsecond_keyword
///     | week_keyword
///     | quarter_keyword
///     | decade_keyword
///     | century_keyword
///     | millennium_keyword
///     | year_keyword TO month_keyword
///     | day_keyword TO hour_keyword
///     | day_keyword TO minute_keyword
///     | day_keyword TO second_keyword
///     | hour_keyword TO minute_keyword
///     | hour_keyword TO second_keyword
///     | minute_keyword TO second_keyword
///     | /* empty */
/// ```
pub(super) fn opt_interval<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // year_keyword
        0 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::YEAR),
            rhs.loc(1),
        )])),
        // month_keyword
        1 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::MONTH),
            rhs.loc(1),
        )])),
        // day_keyword
        2 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::DAY),
            rhs.loc(1),
        )])),
        // hour_keyword
        3 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::HOUR),
            rhs.loc(1),
        )])),
        // minute_keyword
        4 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::MINUTE),
            rhs.loc(1),
        )])),
        // second_keyword
        5 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::SECOND),
            rhs.loc(1),
        )])),
        // millisecond_keyword
        6 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::MILLISECOND),
            rhs.loc(1),
        )])),
        // microsecond_keyword
        7 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::MICROSECOND),
            rhs.loc(1),
        )])),
        // week_keyword
        8 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::WEEK),
            rhs.loc(1),
        )])),
        // quarter_keyword
        9 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::QUARTER),
            rhs.loc(1),
        )])),
        // decade_keyword
        10 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::DECADE),
            rhs.loc(1),
        )])),
        // century_keyword
        11 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::CENTURY),
            rhs.loc(1),
        )])),
        // millennium_keyword
        12 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::MILLENNIUM),
            rhs.loc(1),
        )])),
        // year_keyword TO month_keyword
        13 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::YEAR) | interval_mask(interval_field::MONTH),
            rhs.loc(1),
        )])),
        // day_keyword TO hour_keyword
        14 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::DAY) | interval_mask(interval_field::HOUR),
            rhs.loc(1),
        )])),
        // day_keyword TO minute_keyword
        15 => Ok(SemValue::List(vec![make_int_const(
            (interval_mask(interval_field::DAY) | interval_mask(interval_field::HOUR))
                | interval_mask(interval_field::MINUTE),
            rhs.loc(1),
        )])),
        // day_keyword TO second_keyword
        16 => Ok(SemValue::List(vec![make_int_const(
            ((interval_mask(interval_field::DAY) | interval_mask(interval_field::HOUR))
                | interval_mask(interval_field::MINUTE))
                | interval_mask(interval_field::SECOND),
            rhs.loc(1),
        )])),
        // hour_keyword TO minute_keyword
        17 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::HOUR) | interval_mask(interval_field::MINUTE),
            rhs.loc(1),
        )])),
        // hour_keyword TO second_keyword
        18 => Ok(SemValue::List(vec![make_int_const(
            (interval_mask(interval_field::HOUR) | interval_mask(interval_field::MINUTE))
                | interval_mask(interval_field::SECOND),
            rhs.loc(1),
        )])),
        // minute_keyword TO second_keyword
        19 => Ok(SemValue::List(vec![make_int_const(
            interval_mask(interval_field::MINUTE) | interval_mask(interval_field::SECOND),
            rhs.loc(1),
        )])),
        // /* empty */
        20 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// a_expr:
///       c_expr
///     | a_expr TYPECAST Typename
///     | a_expr COLLATE any_name
///     | a_expr AT TIME ZONE a_expr
///     | '+' a_expr
///     | '-' a_expr
///     | a_expr '+' a_expr
///     | a_expr '-' a_expr
///     | a_expr '*' a_expr
///     | a_expr '/' a_expr
///     | a_expr INTEGER_DIVISION a_expr
///     | a_expr '%' a_expr
///     | a_expr '^' a_expr
///     | a_expr POWER_OF a_expr
///     | a_expr '<' a_expr
///     | a_expr '>' a_expr
///     | a_expr '=' a_expr
///     | a_expr LESS_EQUALS a_expr
///     | a_expr GREATER_EQUALS a_expr
///     | a_expr NOT_EQUALS a_expr
///     | a_expr qual_Op a_expr
///     | qual_Op a_expr
///     | a_expr qual_Op
///     | a_expr AND a_expr
///     | a_expr OR a_expr
///     | NOT a_expr
///     | NOT_LA a_expr
///     | a_expr GLOB a_expr
///     | a_expr LIKE a_expr
///     | a_expr LIKE a_expr ESCAPE a_expr
///     | a_expr NOT_LA LIKE a_expr
///     | a_expr NOT_LA LIKE a_expr ESCAPE a_expr
///     | a_expr ILIKE a_expr
///     | a_expr ILIKE a_expr ESCAPE a_expr
///     | a_expr NOT_LA ILIKE a_expr
///     | a_expr NOT_LA ILIKE a_expr ESCAPE a_expr
///     | a_expr SIMILAR TO a_expr
///     | a_expr SIMILAR TO a_expr ESCAPE a_expr
///     | a_expr NOT_LA SIMILAR TO a_expr
///     | a_expr NOT_LA SIMILAR TO a_expr ESCAPE a_expr
///     | a_expr IS NULL_P
///     | a_expr ISNULL
///     | a_expr IS NOT NULL_P
///     | a_expr NOT NULL_P
///     | a_expr NOTNULL
///     | LAMBDA name_list SINGLE_COLON a_expr
///     | a_expr SINGLE_ARROW a_expr
///     | a_expr DOUBLE_ARROW a_expr
///     | row OVERLAPS row
///     | a_expr IS TRUE_P
///     | a_expr IS NOT TRUE_P
///     | a_expr IS FALSE_P
///     | a_expr IS NOT FALSE_P
///     | a_expr IS UNKNOWN
///     | a_expr IS NOT UNKNOWN
///     | a_expr IS DISTINCT FROM a_expr
///     | a_expr IS NOT DISTINCT FROM a_expr
///     | a_expr IS OF '(' type_list ')'
///     | a_expr IS NOT OF '(' type_list ')'
///     | a_expr BETWEEN opt_asymmetric b_expr AND a_expr
///     | a_expr NOT_LA BETWEEN opt_asymmetric b_expr AND a_expr
///     | a_expr BETWEEN SYMMETRIC b_expr AND a_expr
///     | a_expr NOT_LA BETWEEN SYMMETRIC b_expr AND a_expr
///     | a_expr IN_P in_expr
///     | a_expr NOT_LA IN_P in_expr
///     | a_expr subquery_Op sub_type select_with_parens
///     | a_expr subquery_Op sub_type '(' a_expr ')'
///     | DEFAULT
///     | COLUMNS '(' a_expr ')'
///     | UNPACK '(' a_expr ')'
///     | '*' COLUMNS '(' a_expr ')'
///     | '*' opt_except_list opt_replace_list opt_rename_list
///     | ColId '.' '*' opt_except_list opt_replace_list opt_rename_list
/// ```
pub(super) fn a_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr TYPECAST Typename
        1 => Ok(SemValue::node(make_type_cast(
            rhs.node(1)?,
            rhs.ptr::<TypeName>(3)?,
            0,
            rhs.loc(2),
        ))),
        // a_expr COLLATE any_name
        2 => {
            let n = CollateClause {
                arg: rhs.node(1)?,
                collname: rhs.list(3)?,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(n))
        }
        // a_expr AT TIME ZONE a_expr
        3 => Ok(SemValue::node(make_func_call(
            system_func_name("timezone"),
            vec![rhs.required_node(5)?, rhs.required_node(1)?],
            rhs.loc(2),
        ))),
        // '+' a_expr
        4 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "+",
            None,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        // '-' a_expr
        5 => Ok(SemValue::node(do_negate(rhs.node(2)?, rhs.loc(1)))),
        // a_expr '+' a_expr
        6 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "+",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '-' a_expr
        7 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "-",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '*' a_expr
        8 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "*",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '/' a_expr
        9 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "/",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr INTEGER_DIVISION a_expr
        10 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "//",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '%' a_expr
        11 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "%",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '^' a_expr
        12 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "^",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr POWER_OF a_expr
        13 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "**",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '<' a_expr
        14 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "<",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '>' a_expr
        15 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            ">",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr '=' a_expr
        16 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "=",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr LESS_EQUALS a_expr
        17 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "<=",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr GREATER_EQUALS a_expr
        18 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            ">=",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr NOT_EQUALS a_expr
        19 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "<>",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr qual_Op a_expr
        20 => Ok(SemValue::node(make_a_expr(
            AExprKind::Op,
            rhs.list(2)?,
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // qual_Op a_expr
        21 => Ok(SemValue::node(make_a_expr(
            AExprKind::Op,
            rhs.list(1)?,
            None,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        // a_expr qual_Op
        22 => Ok(SemValue::node(make_a_expr(
            AExprKind::Op,
            rhs.list(2)?,
            rhs.node(1)?,
            None,
            rhs.loc(2),
        ))),
        // a_expr AND a_expr
        23 => Ok(SemValue::node(make_and_expr(
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        )?)),
        // a_expr OR a_expr
        24 => Ok(SemValue::node(make_or_expr(
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        )?)),
        // NOT a_expr
        // NOT_LA a_expr
        25 | 26 => Ok(SemValue::node(make_not_expr(rhs.node(2)?, rhs.loc(1))?)),
        // a_expr GLOB a_expr
        27 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Glob,
            "~~~",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr LIKE a_expr
        28 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Like,
            "~~",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr LIKE a_expr ESCAPE a_expr
        29 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("like_escape"),
                vec![
                    rhs.required_node(1)?,
                    rhs.required_node(3)?,
                    rhs.required_node(5)?,
                ],
                rhs.loc(2),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // a_expr NOT_LA LIKE a_expr
        30 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Like,
            "!~~",
            rhs.node(1)?,
            rhs.node(4)?,
            rhs.loc(2),
        ))),
        // a_expr NOT_LA LIKE a_expr ESCAPE a_expr
        31 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("not_like_escape"),
                vec![
                    rhs.required_node(1)?,
                    rhs.required_node(4)?,
                    rhs.required_node(6)?,
                ],
                rhs.loc(2),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // a_expr ILIKE a_expr
        32 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Ilike,
            "~~*",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // a_expr ILIKE a_expr ESCAPE a_expr
        33 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("ilike_escape"),
                vec![
                    rhs.required_node(1)?,
                    rhs.required_node(3)?,
                    rhs.required_node(5)?,
                ],
                rhs.loc(2),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // a_expr NOT_LA ILIKE a_expr
        34 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Ilike,
            "!~~*",
            rhs.node(1)?,
            rhs.node(4)?,
            rhs.loc(2),
        ))),
        // a_expr NOT_LA ILIKE a_expr ESCAPE a_expr
        35 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("not_ilike_escape"),
                vec![
                    rhs.required_node(1)?,
                    rhs.required_node(4)?,
                    rhs.required_node(6)?,
                ],
                rhs.loc(2),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // a_expr SIMILAR TO a_expr
        36 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("similar_escape"),
                vec![rhs.required_node(4)?, make_null_a_const(-1)],
                rhs.loc(2),
            )));
            Ok(SemValue::node(make_simple_a_expr(
                AExprKind::Similar,
                "~",
                rhs.node(1)?,
                n.map(Node::from),
                rhs.loc(2),
            )))
        }
        // a_expr SIMILAR TO a_expr ESCAPE a_expr
        37 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("similar_escape"),
                vec![rhs.required_node(4)?, rhs.required_node(6)?],
                rhs.loc(2),
            )));
            Ok(SemValue::node(make_simple_a_expr(
                AExprKind::Similar,
                "~",
                rhs.node(1)?,
                n.map(Node::from),
                rhs.loc(2),
            )))
        }
        // a_expr NOT_LA SIMILAR TO a_expr
        38 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("similar_escape"),
                vec![rhs.required_node(5)?, make_null_a_const(-1)],
                rhs.loc(2),
            )));
            Ok(SemValue::node(make_simple_a_expr(
                AExprKind::Similar,
                "!~",
                rhs.node(1)?,
                n.map(Node::from),
                rhs.loc(2),
            )))
        }
        // a_expr NOT_LA SIMILAR TO a_expr ESCAPE a_expr
        39 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("similar_escape"),
                vec![rhs.required_node(5)?, rhs.required_node(7)?],
                rhs.loc(2),
            )));
            Ok(SemValue::node(make_simple_a_expr(
                AExprKind::Similar,
                "!~",
                rhs.node(1)?,
                n.map(Node::from),
                rhs.loc(2),
            )))
        }
        // a_expr IS NULL_P
        // a_expr ISNULL
        40 | 41 => {
            let n = NullTest {
                arg: rhs.node(1)?,
                nulltesttype: NullTestType::Null,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // a_expr IS NOT NULL_P
        // a_expr NOT NULL_P
        // a_expr NOTNULL
        42..=44 => {
            let n = NullTest {
                arg: rhs.node(1)?,
                nulltesttype: NullTestType::NotNull,
                location: rhs.loc(2),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // LAMBDA name_list SINGLE_COLON a_expr
        45 => {
            let n = LambdaFunction {
                lhs: rhs.list(2)?,
                rhs: rhs.node(4)?,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(n))
        }
        // a_expr SINGLE_ARROW a_expr
        46 => {
            let n = SingleArrowFunction {
                lhs: rhs.node(1)?,
                rhs: rhs.node(3)?,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(n))
        }
        // a_expr DOUBLE_ARROW a_expr
        47 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "->>",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // row OVERLAPS row
        48 => {
            let row1 = rhs.list(1)?;
            let row3 = rhs.list(3)?;
            if row1.len() != 2 {
                return Err(rhs.error_at(
                    "wrong number of parameters on left side of OVERLAPS expression",
                    rhs.loc(1),
                ));
            }
            if row3.len() != 2 {
                return Err(rhs.error_at(
                    "wrong number of parameters on right side of OVERLAPS expression",
                    rhs.loc(3),
                ));
            }
            Ok(SemValue::node(make_func_call(
                system_func_name("overlaps"),
                concat(row1, row3),
                rhs.loc(2),
            )))
        }
        // a_expr IS TRUE_P
        49 => {
            let b = BooleanTest {
                arg: rhs.node(1)?,
                booltesttype: BoolTestType::True,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(b))
        }
        // a_expr IS NOT TRUE_P
        50 => {
            let b = BooleanTest {
                arg: rhs.node(1)?,
                booltesttype: BoolTestType::NotTrue,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(b))
        }
        // a_expr IS FALSE_P
        51 => {
            let b = BooleanTest {
                arg: rhs.node(1)?,
                booltesttype: BoolTestType::False,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(b))
        }
        // a_expr IS NOT FALSE_P
        52 => {
            let b = BooleanTest {
                arg: rhs.node(1)?,
                booltesttype: BoolTestType::NotFalse,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(b))
        }
        // a_expr IS UNKNOWN
        53 => {
            let b = BooleanTest {
                arg: rhs.node(1)?,
                booltesttype: BoolTestType::Unknown,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(b))
        }
        // a_expr IS NOT UNKNOWN
        54 => {
            let b = BooleanTest {
                arg: rhs.node(1)?,
                booltesttype: BoolTestType::NotUnknown,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(b))
        }
        // a_expr IS DISTINCT FROM a_expr
        55 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Distinct,
            "=",
            rhs.node(1)?,
            rhs.node(5)?,
            rhs.loc(2),
        ))),
        // a_expr IS NOT DISTINCT FROM a_expr
        56 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::NotDistinct,
            "=",
            rhs.node(1)?,
            rhs.node(6)?,
            rhs.loc(2),
        ))),
        // a_expr IS OF '(' type_list ')'
        57 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Of,
            "=",
            rhs.node(1)?,
            Node::from_list(rhs.list(5)?),
            rhs.loc(2),
        ))),
        // a_expr IS NOT OF '(' type_list ')'
        58 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Of,
            "<>",
            rhs.node(1)?,
            Node::from_list(rhs.list(6)?),
            rhs.loc(2),
        ))),
        // a_expr BETWEEN opt_asymmetric b_expr AND a_expr
        59 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Between,
            "BETWEEN",
            rhs.node(1)?,
            Node::from_list(vec![rhs.required_node(4)?, rhs.required_node(6)?]),
            rhs.loc(2),
        ))),
        // a_expr NOT_LA BETWEEN opt_asymmetric b_expr AND a_expr
        60 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::NotBetween,
            "NOT BETWEEN",
            rhs.node(1)?,
            Node::from_list(vec![rhs.required_node(5)?, rhs.required_node(7)?]),
            rhs.loc(2),
        ))),
        // a_expr BETWEEN SYMMETRIC b_expr AND a_expr
        61 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::BetweenSym,
            "BETWEEN SYMMETRIC",
            rhs.node(1)?,
            Node::from_list(vec![rhs.required_node(4)?, rhs.required_node(6)?]),
            rhs.loc(2),
        ))),
        // a_expr NOT_LA BETWEEN SYMMETRIC b_expr AND a_expr
        62 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::NotBetweenSym,
            "NOT BETWEEN SYMMETRIC",
            rhs.node(1)?,
            Node::from_list(vec![rhs.required_node(5)?, rhs.required_node(7)?]),
            rhs.loc(2),
        ))),
        // a_expr IN_P in_expr
        63 => {
            // in_expr returns a SubLink or a list of a_exprs
            let expr = match rhs.node(3)? {
                Some(Node::SubLink(mut sublink)) => {
                    // generate foo = ANY (subquery)
                    sublink.sub_link_type = SubLinkType::Any;
                    sublink.testexpr = rhs.node(1)?;
                    sublink.oper_name = List::new(); // show it's IN not = ANY
                    sublink.location = rhs.loc(2);
                    Node::SubLink(sublink)
                }
                // generate scalar IN expression
                in_expr => make_simple_a_expr(AExprKind::In, "=", rhs.node(1)?, in_expr, rhs.loc(2)).into(),
            };
            Ok(SemValue::node(expr))
        }
        // a_expr NOT_LA IN_P in_expr
        64 => {
            // in_expr returns a SubLink or a list of a_exprs
            let expr = match rhs.node(4)? {
                Some(Node::SubLink(mut sublink)) => {
                    // generate NOT (foo = ANY (subquery))
                    sublink.sub_link_type = SubLinkType::Any;
                    sublink.testexpr = rhs.node(1)?;
                    sublink.oper_name = List::new(); // show it's IN not = ANY
                    sublink.location = rhs.loc(2);
                    // stick a NOT on top; must have same parse location
                    make_not_expr(Some(Node::SubLink(sublink)), rhs.loc(2))?
                }
                // generate scalar NOT IN expression
                in_expr => make_simple_a_expr(AExprKind::In, "<>", rhs.node(1)?, in_expr, rhs.loc(2)).into(),
            };
            Ok(SemValue::node(expr))
        }
        // a_expr subquery_Op sub_type select_with_parens
        65 => {
            let n = SubLink {
                sub_link_type: rhs.sub_link_type(3)?,
                testexpr: rhs.node(1)?,
                oper_name: rhs.list(2)?,
                subselect: rhs.node(4)?,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(n))
        }
        // a_expr subquery_Op sub_type '(' a_expr ')'
        66 => {
            let kind = if rhs.sub_link_type(3)? == SubLinkType::Any {
                AExprKind::OpAny
            } else {
                AExprKind::OpAll
            };
            let expr = make_a_expr(kind, rhs.list(2)?, rhs.node(1)?, rhs.node(5)?, rhs.loc(2));
            Ok(SemValue::node(expr))
        }
        // DEFAULT
        67 => {
            let n = SetToDefault {
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // COLUMNS '(' a_expr ')'
        68 => {
            let star = AStar {
                expr: rhs.node(3)?,
                columns: true,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(star))
        }
        // UNPACK '(' a_expr ')'
        69 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("unpack"),
                vec![rhs.required_node(3)?],
                rhs.loc(1),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // '*' COLUMNS '(' a_expr ')'
        70 => {
            let star = AStar {
                expr: rhs.node(4)?,
                columns: true,
                location: rhs.loc(1),
                ..Default::default()
            };
            let n = Some(Box::new(make_func_call(
                system_func_name("unpack"),
                vec![Node::from(star)],
                rhs.loc(1),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // '*' opt_except_list opt_replace_list opt_rename_list
        71 => {
            let star = AStar {
                except_list: rhs.list(2)?,
                replace_list: rhs.list(3)?,
                rename_list: rhs.list(4)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(star))
        }
        // ColId '.' '*' opt_except_list opt_replace_list opt_rename_list
        72 => {
            let star = AStar {
                relation: rhs.str(1)?,
                except_list: rhs.list(4)?,
                replace_list: rhs.list(5)?,
                rename_list: rhs.list(6)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(star))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// b_expr:
///       c_expr
///     | b_expr TYPECAST Typename
///     | '+' b_expr
///     | '-' b_expr
///     | b_expr '+' b_expr
///     | b_expr '-' b_expr
///     | b_expr '*' b_expr
///     | b_expr '/' b_expr
///     | b_expr INTEGER_DIVISION b_expr
///     | b_expr '%' b_expr
///     | b_expr '^' b_expr
///     | b_expr POWER_OF b_expr
///     | b_expr '<' b_expr
///     | b_expr '>' b_expr
///     | b_expr '=' b_expr
///     | b_expr LESS_EQUALS b_expr
///     | b_expr GREATER_EQUALS b_expr
///     | b_expr NOT_EQUALS b_expr
///     | b_expr qual_Op b_expr
///     | qual_Op b_expr
///     | b_expr qual_Op
///     | b_expr IS DISTINCT FROM b_expr
///     | b_expr IS NOT DISTINCT FROM b_expr
///     | b_expr IS OF '(' type_list ')'
///     | b_expr IS NOT OF '(' type_list ')'
/// ```
pub(super) fn b_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // b_expr TYPECAST Typename
        1 => Ok(SemValue::node(make_type_cast(
            rhs.node(1)?,
            rhs.ptr::<TypeName>(3)?,
            0,
            rhs.loc(2),
        ))),
        // '+' b_expr
        2 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "+",
            None,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        // '-' b_expr
        3 => Ok(SemValue::node(do_negate(rhs.node(2)?, rhs.loc(1)))),
        // b_expr '+' b_expr
        4 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "+",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '-' b_expr
        5 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "-",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '*' b_expr
        6 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "*",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '/' b_expr
        7 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "/",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr INTEGER_DIVISION b_expr
        8 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "//",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '%' b_expr
        9 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "%",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '^' b_expr
        10 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "^",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr POWER_OF b_expr
        11 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "**",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '<' b_expr
        12 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "<",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '>' b_expr
        13 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            ">",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr '=' b_expr
        14 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "=",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr LESS_EQUALS b_expr
        15 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "<=",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr GREATER_EQUALS b_expr
        16 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            ">=",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr NOT_EQUALS b_expr
        17 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Op,
            "<>",
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // b_expr qual_Op b_expr
        18 => Ok(SemValue::node(make_a_expr(
            AExprKind::Op,
            rhs.list(2)?,
            rhs.node(1)?,
            rhs.node(3)?,
            rhs.loc(2),
        ))),
        // qual_Op b_expr
        19 => Ok(SemValue::node(make_a_expr(
            AExprKind::Op,
            rhs.list(1)?,
            None,
            rhs.node(2)?,
            rhs.loc(1),
        ))),
        // b_expr qual_Op
        20 => Ok(SemValue::node(make_a_expr(
            AExprKind::Op,
            rhs.list(2)?,
            rhs.node(1)?,
            None,
            rhs.loc(2),
        ))),
        // b_expr IS DISTINCT FROM b_expr
        21 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Distinct,
            "=",
            rhs.node(1)?,
            rhs.node(5)?,
            rhs.loc(2),
        ))),
        // b_expr IS NOT DISTINCT FROM b_expr
        22 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::NotDistinct,
            "=",
            rhs.node(1)?,
            rhs.node(6)?,
            rhs.loc(2),
        ))),
        // b_expr IS OF '(' type_list ')'
        23 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Of,
            "=",
            rhs.node(1)?,
            Node::from_list(rhs.list(5)?),
            rhs.loc(2),
        ))),
        // b_expr IS NOT OF '(' type_list ')'
        24 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Of,
            "<>",
            rhs.node(1)?,
            Node::from_list(rhs.list(6)?),
            rhs.loc(2),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// c_expr:
///       d_expr
///     | indirection_expr_or_a_expr opt_extended_indirection
/// ```
pub(super) fn c_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // indirection_expr_or_a_expr opt_extended_indirection
        1 => {
            let indirection = rhs.list(2)?;
            if indirection.is_empty() {
                return Ok(SemValue::Node(rhs.node(1)?));
            }
            let indirection = check_indirection(indirection, rhs.scanner())?;
            Ok(SemValue::node(AIndirection {
                arg: rhs.node(1)?,
                indirection,
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// d_expr:
///       columnref_opt_indirection
///     | AexprConst
///     | select_with_parens
///     | select_with_parens indirection
///     | EXISTS select_with_parens
///     | grouping_or_grouping_id '(' expr_list_opt_comma ')'
/// ```
pub(super) fn d_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // select_with_parens
        2 => {
            let n = SubLink {
                sub_link_type: SubLinkType::Expr,
                testexpr: None,
                oper_name: List::new(),
                subselect: rhs.node(1)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(n))
        }
        // select_with_parens indirection
        3 => {
            let mut n = SubLink { ..Default::default() };
            let mut a = AIndirection { ..Default::default() };
            n.sub_link_type = SubLinkType::Expr;
            n.testexpr = None;
            n.oper_name = List::new();
            n.subselect = rhs.node(1)?;
            n.location = rhs.loc(1);
            a.arg = Some(n.into());
            a.indirection = check_indirection(rhs.list(2)?, rhs.scanner())?;
            Ok(SemValue::node(a))
        }
        // EXISTS select_with_parens
        4 => {
            let n = SubLink {
                sub_link_type: SubLinkType::Exists,
                testexpr: None,
                oper_name: List::new(),
                subselect: rhs.node(2)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(n))
        }
        // grouping_or_grouping_id '(' expr_list_opt_comma ')'
        5 => {
            let g = GroupingFunc {
                args: rhs.list(3)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(g))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// indirection_expr_or_a_expr:
///       '(' a_expr ')'
///     | indirection_expr
///     | row
/// ```
pub(super) fn indirection_expr_or_a_expr<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // row
        2 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("row"),
                rhs.list(1)?,
                rhs.loc(1),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// param_expr:
///       '?'
///     | PARAM
///     | '$' ColLabel
/// ```
pub(super) fn param_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '?'
        0 => Ok(SemValue::node(make_param_ref(0, rhs.loc(1)))),
        // PARAM
        1 => {
            let p = ParamRef {
                number: rhs.int(1)?,
                location: rhs.loc(1),
                ..Default::default()
            };
            Ok(SemValue::node(p))
        }
        // '$' ColLabel
        2 => Ok(SemValue::node(make_named_param_ref(rhs.string(2)?, rhs.loc(1)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// indirection_expr:
///       param_expr
///     | struct_expr
///     | map_expr
///     | func_expr
///     | case_expr
///     | list_expr
///     | list_comprehension
///     | ARRAY select_with_parens
///     | ARRAY '[' opt_expr_list_opt_comma ']'
///     | '#' ICONST
/// ```
pub(super) fn indirection_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ARRAY select_with_parens
        7 => {
            let n = SubLink {
                sub_link_type: SubLinkType::Array,
                testexpr: None,
                oper_name: List::new(),
                subselect: rhs.node(2)?,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(n))
        }
        // ARRAY '[' opt_expr_list_opt_comma ']'
        8 => {
            let func_name = vec![Node::string("construct_array")];
            let n = Some(Box::new(make_func_call(func_name, rhs.list(3)?, rhs.loc(1))));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // '#' ICONST
        9 => {
            let n = PositionalReference {
                position: rhs.int(2)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// list_expr:
///       '[' opt_expr_list_opt_comma ']'
/// ```
pub(super) fn list_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '[' opt_expr_list_opt_comma ']'
        0 => {
            let n = Some(Box::new(make_func_call(
                system_func_name("list_value"),
                rhs.list(2)?,
                rhs.loc(2),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// struct_expr:
///       '{' dict_arguments_opt_comma '}'
/// ```
pub(super) fn struct_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '{' dict_arguments_opt_comma '}'
        0 => {
            let f = Some(Box::new(make_func_call(
                system_func_name("struct_pack"),
                rhs.list(2)?,
                rhs.loc(2),
            )));
            Ok(SemValue::Node(f.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// map_expr:
///       MAP '{' opt_map_arguments_opt_comma '}'
/// ```
pub(super) fn map_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // MAP '{' opt_map_arguments_opt_comma '}'
        0 => {
            let mut keys = List::new();
            let mut values = List::new();
            for entry in rhs.list(3)? {
                // each entry is a (key, value) pair; C takes the list's head and tail
                let Node::List(mut pair) = entry else {
                    return Err(internal("map entry is not a list"));
                };
                let value = required(pair.pop())?;
                let key = if pair.is_empty() {
                    value.clone()
                } else {
                    pair.swap_remove(0)
                };
                keys.push(key);
                values.push(value);
            }
            let keys = make_func_call(system_func_name("list_value"), keys, rhs.loc(3));
            let values = make_func_call(system_func_name("list_value"), values, rhs.loc(3));
            let map = make_func_call(
                system_func_name("map"),
                vec![keys.into(), values.into()],
                rhs.loc(3),
            );
            Ok(SemValue::node(map))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_application:
///       func_name '(' ')'
///     | func_name '(' sort_clause opt_ignore_nulls ')'
///     | func_name '(' func_arg_list opt_sort_clause opt_ignore_nulls ')'
///     | func_name '(' VARIADIC func_arg_expr opt_sort_clause opt_ignore_nulls ')'
///     | func_name '(' func_arg_list ',' VARIADIC func_arg_expr opt_sort_clause opt_ignore_nulls ')'
///     | func_name '(' ALL func_arg_list opt_sort_clause opt_ignore_nulls ')'
///     | func_name '(' DISTINCT func_arg_list opt_sort_clause opt_ignore_nulls ')'
/// ```
pub(super) fn func_application<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // func_name '(' ')'
        0 => Ok(SemValue::node(make_func_call(
            rhs.list(1)?,
            List::new(),
            rhs.loc(1),
        ))),
        // func_name '(' sort_clause opt_ignore_nulls ')'
        1 => {
            let mut n = Some(Box::new(make_func_call(rhs.list(1)?, List::new(), rhs.loc(1))));
            required_mut(&mut n)?.agg_order = rhs.list(3)?;
            required_mut(&mut n)?.agg_ignore_nulls = rhs.ignore_nulls(4)?;
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // func_name '(' func_arg_list opt_sort_clause opt_ignore_nulls ')'
        2 => {
            let mut n = Some(Box::new(make_func_call(rhs.list(1)?, rhs.list(3)?, rhs.loc(1))));
            required_mut(&mut n)?.agg_order = rhs.list(4)?;
            required_mut(&mut n)?.agg_ignore_nulls = rhs.ignore_nulls(5)?;
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // func_name '(' VARIADIC func_arg_expr opt_sort_clause opt_ignore_nulls ')'
        3 => {
            let mut n = Some(Box::new(make_func_call(
                rhs.list(1)?,
                vec![rhs.required_node(4)?],
                rhs.loc(1),
            )));
            required_mut(&mut n)?.func_variadic = true;
            required_mut(&mut n)?.agg_order = rhs.list(5)?;
            required_mut(&mut n)?.agg_ignore_nulls = rhs.ignore_nulls(6)?;
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // func_name '(' func_arg_list ',' VARIADIC func_arg_expr opt_sort_clause opt_ignore_nulls ')'
        4 => {
            let mut n = Some(Box::new(make_func_call(
                rhs.list(1)?,
                append(rhs.list(3)?, rhs.required_node(6)?),
                rhs.loc(1),
            )));
            required_mut(&mut n)?.func_variadic = true;
            required_mut(&mut n)?.agg_order = rhs.list(7)?;
            required_mut(&mut n)?.agg_ignore_nulls = rhs.ignore_nulls(8)?;
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // func_name '(' ALL func_arg_list opt_sort_clause opt_ignore_nulls ')'
        5 => {
            let mut n = Some(Box::new(make_func_call(rhs.list(1)?, rhs.list(4)?, rhs.loc(1))));
            required_mut(&mut n)?.agg_order = rhs.list(5)?;
            required_mut(&mut n)?.agg_ignore_nulls = rhs.ignore_nulls(6)?;
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // func_name '(' DISTINCT func_arg_list opt_sort_clause opt_ignore_nulls ')'
        6 => {
            let mut n = Some(Box::new(make_func_call(rhs.list(1)?, rhs.list(4)?, rhs.loc(1))));
            required_mut(&mut n)?.agg_order = rhs.list(5)?;
            required_mut(&mut n)?.agg_ignore_nulls = rhs.ignore_nulls(6)?;
            required_mut(&mut n)?.agg_distinct = true;
            Ok(SemValue::Node(n.map(Node::from)))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_expr:
///       func_application within_group_clause filter_clause export_clause over_clause
///     | func_expr_common_subexpr
/// ```
pub(super) fn func_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // func_application within_group_clause filter_clause export_clause over_clause
        0 => {
            let within_group_clause = rhs.list(2)?;
            let mut n = rhs.take::<FuncCall>(1)?;
            if !within_group_clause.is_empty() {
                if !n.agg_order.is_empty() {
                    return Err(rhs.error_at(
                        "cannot use multiple ORDER BY clauses with WITHIN GROUP",
                        rhs.loc(2),
                    ));
                }
                if n.agg_distinct {
                    return Err(rhs.error_at("cannot use DISTINCT with WITHIN GROUP", rhs.loc(2)));
                }
                if n.func_variadic {
                    return Err(rhs.error_at("cannot use VARIADIC with WITHIN GROUP", rhs.loc(2)));
                }
                n.agg_order = within_group_clause;
                n.agg_within_group = true;
            }
            n.agg_filter = rhs.node(3)?;
            n.export_state = rhs.bool(4)?;
            n.over = rhs.ptr::<WindowDef>(5)?;
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_expr_common_subexpr:
///       COLLATION FOR '(' a_expr ')'
///     | CAST '(' a_expr AS Typename ')'
///     | TRY_CAST '(' a_expr AS Typename ')'
///     | EXTRACT '(' extract_list ')'
///     | OVERLAY '(' overlay_list ')'
///     | POSITION '(' position_list ')'
///     | SUBSTRING '(' substr_list ')'
///     | TREAT '(' a_expr AS Typename ')'
///     | TRIM '(' BOTH trim_list ')'
///     | TRIM '(' LEADING trim_list ')'
///     | TRIM '(' TRAILING trim_list ')'
///     | TRIM '(' trim_list ')'
///     | NULLIF '(' a_expr ',' a_expr ')'
///     | COALESCE '(' expr_list_opt_comma ')'
/// ```
pub(super) fn func_expr_common_subexpr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // COLLATION FOR '(' a_expr ')'
        0 => Ok(SemValue::node(make_func_call(
            system_func_name("pg_collation_for"),
            vec![rhs.required_node(4)?],
            rhs.loc(1),
        ))),
        // CAST '(' a_expr AS Typename ')'
        1 => Ok(SemValue::node(make_type_cast(
            rhs.node(3)?,
            rhs.ptr::<TypeName>(5)?,
            0,
            rhs.loc(1),
        ))),
        // TRY_CAST '(' a_expr AS Typename ')'
        2 => Ok(SemValue::node(make_type_cast(
            rhs.node(3)?,
            rhs.ptr::<TypeName>(5)?,
            1,
            rhs.loc(1),
        ))),
        // EXTRACT '(' extract_list ')'
        3 => Ok(SemValue::node(make_func_call(
            system_func_name("date_part"),
            rhs.list(3)?,
            rhs.loc(1),
        ))),
        // OVERLAY '(' overlay_list ')'
        4 => Ok(SemValue::node(make_func_call(
            system_func_name("overlay"),
            rhs.list(3)?,
            rhs.loc(1),
        ))),
        // POSITION '(' position_list ')'
        5 => Ok(SemValue::node(make_func_call(
            system_func_name("__internal_position_operator"),
            rhs.list(3)?,
            rhs.loc(1),
        ))),
        // SUBSTRING '(' substr_list ')'
        6 => Ok(SemValue::node(make_func_call(
            system_func_name("substring"),
            rhs.list(3)?,
            rhs.loc(1),
        ))),
        // TREAT '(' a_expr AS Typename ')'
        7 => {
            // TREAT(expr AS type) calls the function named like the type
            let type_name = rhs.take::<TypeName>(5)?;
            let name = required(type_name.names.last()).and_then(str_val)?;
            let call = make_func_call(system_func_name(&name), vec![rhs.required_node(3)?], rhs.loc(1));
            Ok(SemValue::node(call))
        }
        // TRIM '(' BOTH trim_list ')'
        8 => Ok(SemValue::node(make_func_call(
            system_func_name("trim"),
            rhs.list(4)?,
            rhs.loc(1),
        ))),
        // TRIM '(' LEADING trim_list ')'
        9 => Ok(SemValue::node(make_func_call(
            system_func_name("ltrim"),
            rhs.list(4)?,
            rhs.loc(1),
        ))),
        // TRIM '(' TRAILING trim_list ')'
        10 => Ok(SemValue::node(make_func_call(
            system_func_name("rtrim"),
            rhs.list(4)?,
            rhs.loc(1),
        ))),
        // TRIM '(' trim_list ')'
        11 => Ok(SemValue::node(make_func_call(
            system_func_name("trim"),
            rhs.list(3)?,
            rhs.loc(1),
        ))),
        // NULLIF '(' a_expr ',' a_expr ')'
        12 => Ok(SemValue::node(make_simple_a_expr(
            AExprKind::Nullif,
            "=",
            rhs.node(3)?,
            rhs.node(5)?,
            rhs.loc(1),
        ))),
        // COALESCE '(' expr_list_opt_comma ')'
        13 => {
            let c = CoalesceExpr {
                args: rhs.list(3)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(c))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// list_comprehension:
///       '[' a_expr FOR name_list IN_P a_expr ']'
///     | '[' a_expr FOR name_list IN_P c_expr IF_P a_expr ']'
/// ```
pub(super) fn list_comprehension<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '[' a_expr FOR name_list IN_P a_expr ']'
        0 => {
            let lambda = LambdaFunction {
                lhs: rhs.list(4)?,
                rhs: rhs.node(2)?,
                location: rhs.loc(1),
            };
            let n = Some(Box::new(make_func_call(
                system_func_name("list_apply"),
                vec![rhs.required_node(6)?, lambda.into()],
                rhs.loc(1),
            )));
            Ok(SemValue::Node(n.map(Node::from)))
        }
        // '[' a_expr FOR name_list IN_P c_expr IF_P a_expr ']'
        1 => {
            // [result FOR names IN source IF filter] becomes
            // list_apply(list_filter(list_apply(source, names -> {filter: .., result: ..}),
            //                        elem -> elem.filter),
            //            elem -> elem.result)
            let names = rhs.list(4)?;
            let location = rhs.loc(1);
            let filter_expr = NamedArgExpr {
                arg: rhs.node(8)?,
                name: Some("filter".into()),
                argnumber: -1,
                location: rhs.loc(8),
            };
            let result_expr = NamedArgExpr {
                arg: rhs.node(2)?,
                name: Some("result".into()),
                argnumber: -1,
                location: rhs.loc(2),
            };
            let struct_pack = make_func_call(
                system_func_name("struct_pack"),
                vec![filter_expr.into(), result_expr.into()],
                location,
            );
            let lambda_apply_1 = LambdaFunction {
                lhs: names,
                rhs: Some(struct_pack.into()),
                location,
            };
            let apply_func_1 = make_func_call(
                system_func_name("list_apply"),
                vec![rhs.required_node(6)?, lambda_apply_1.into()],
                location,
            );
            // the C code builds both struct_extract calls on the same `elem` column reference
            let elem = make_column_ref("elem", List::new(), location, rhs.scanner())?;
            let filter_name = make_string_const("filter", location);
            let filter_extract = make_func_call(
                system_func_name("struct_extract"),
                vec![elem.clone(), filter_name],
                location,
            );
            let lambda_filter = LambdaFunction {
                lhs: vec![Node::string("elem")],
                rhs: Some(filter_extract.into()),
                location,
            };
            let filter_func = make_func_call(
                system_func_name("list_filter"),
                vec![apply_func_1.into(), lambda_filter.into()],
                location,
            );
            let result_name = make_string_const("result", location);
            let result_extract = make_func_call(
                system_func_name("struct_extract"),
                vec![elem, result_name],
                location,
            );
            let lambda_apply_2 = LambdaFunction {
                lhs: vec![Node::string("elem")],
                rhs: Some(result_extract.into()),
                location,
            };
            let apply_func_2 = make_func_call(
                system_func_name("list_apply"),
                vec![filter_func.into(), lambda_apply_2.into()],
                location,
            );
            Ok(SemValue::node(apply_func_2))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// within_group_clause:
///       WITHIN GROUP_P '(' sort_clause ')'
///     | /* empty */
/// ```
pub(super) fn within_group_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// filter_clause:
///       FILTER '(' WHERE a_expr ')'
///     | FILTER '(' a_expr ')'
///     | /* empty */
/// ```
pub(super) fn filter_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        2 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// export_clause:
///       EXPORT_STATE
///     | /* empty */
/// ```
pub(super) fn export_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // EXPORT_STATE
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// window_clause:
///       WINDOW window_definition_list
///     | /* empty */
/// ```
pub(super) fn window_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// window_definition_list:
///       window_definition
///     | window_definition_list ',' window_definition
/// ```
pub(super) fn window_definition_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // window_definition
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // window_definition_list ',' window_definition
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// window_definition:
///       ColId AS window_specification
/// ```
pub(super) fn window_definition<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId AS window_specification
        0 => {
            let mut n = rhs.take::<WindowDef>(3)?;
            n.name = rhs.str(1)?;
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// over_clause:
///       OVER window_specification
///     | OVER ColId
///     | /* empty */
/// ```
pub(super) fn over_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // OVER ColId
        1 => {
            let n = WindowDef {
                name: rhs.str(2)?,
                refname: None,
                partition_clause: List::new(),
                order_clause: List::new(),
                frame_options: frame_option::DEFAULTS,
                start_offset: None,
                end_offset: None,
                location: rhs.loc(2),
            };
            Ok(SemValue::node(n))
        }
        // /* empty */
        2 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// window_specification:
///       '(' opt_existing_window_name opt_partition_clause opt_sort_clause opt_frame_clause ')'
/// ```
pub(super) fn window_specification<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' opt_existing_window_name opt_partition_clause opt_sort_clause opt_frame_clause ')'
        0 => {
            let opt_frame_clause = rhs.take::<WindowDef>(5)?;
            let n = WindowDef {
                name: None,
                refname: rhs.str(2)?,
                partition_clause: rhs.list(3)?,
                order_clause: rhs.list(4)?,
                frame_options: opt_frame_clause.frame_options,
                start_offset: opt_frame_clause.start_offset,
                end_offset: opt_frame_clause.end_offset,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_existing_window_name:
///       ColId
///     | /* empty */
/// ```
pub(super) fn opt_existing_window_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Str(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_partition_clause:
///       PARTITION BY expr_list
///     | /* empty */
/// ```
pub(super) fn opt_partition_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_frame_clause:
///       RANGE frame_extent opt_window_exclusion_clause
///     | ROWS frame_extent opt_window_exclusion_clause
///     | GROUPS frame_extent opt_window_exclusion_clause
///     | /* empty */
/// ```
pub(super) fn opt_frame_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // RANGE frame_extent opt_window_exclusion_clause
        0 => {
            let mut n = rhs.take::<WindowDef>(2)?;
            n.frame_options |= frame_option::NONDEFAULT | frame_option::RANGE;
            n.frame_options |= rhs.int(3)?;
            Ok(SemValue::node(n))
        }
        // ROWS frame_extent opt_window_exclusion_clause
        1 => {
            let mut n = rhs.take::<WindowDef>(2)?;
            n.frame_options |= frame_option::NONDEFAULT | frame_option::ROWS;
            n.frame_options |= rhs.int(3)?;
            Ok(SemValue::node(n))
        }
        // GROUPS frame_extent opt_window_exclusion_clause
        2 => {
            let mut n = rhs.take::<WindowDef>(2)?;
            n.frame_options |= frame_option::NONDEFAULT | frame_option::GROUPS;
            n.frame_options |= rhs.int(3)?;
            Ok(SemValue::node(n))
        }
        // /* empty */
        3 => {
            let n = WindowDef {
                frame_options: frame_option::DEFAULTS,
                start_offset: None,
                end_offset: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// frame_extent:
///       frame_bound
///     | BETWEEN frame_bound AND frame_bound
/// ```
pub(super) fn frame_extent<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // frame_bound
        0 => {
            let mut n = rhs.take::<WindowDef>(1)?;
            if (n.frame_options & frame_option::START_UNBOUNDED_FOLLOWING) != 0 {
                return Err(rhs.error_at("frame start cannot be UNBOUNDED FOLLOWING", rhs.loc(1)));
            }
            if (n.frame_options & frame_option::START_OFFSET_FOLLOWING) != 0 {
                return Err(rhs.error_at(
                    "frame starting from following row cannot end with current row",
                    rhs.loc(1),
                ));
            }
            n.frame_options |= frame_option::END_CURRENT_ROW;
            Ok(SemValue::node(n))
        }
        // BETWEEN frame_bound AND frame_bound
        1 => {
            let mut n1 = rhs.take::<WindowDef>(2)?;
            let n2 = rhs.take::<WindowDef>(4)?;
            let mut frame_options = n1.frame_options;
            frame_options |= n2.frame_options << 1;
            frame_options |= frame_option::BETWEEN;
            if (frame_options & frame_option::START_UNBOUNDED_FOLLOWING) != 0 {
                return Err(rhs.error_at("frame start cannot be UNBOUNDED FOLLOWING", rhs.loc(2)));
            }
            if (frame_options & frame_option::END_UNBOUNDED_PRECEDING) != 0 {
                return Err(rhs.error_at("frame end cannot be UNBOUNDED PRECEDING", rhs.loc(4)));
            }
            if (frame_options & frame_option::START_CURRENT_ROW) != 0
                && (frame_options & frame_option::END_OFFSET_PRECEDING) != 0
            {
                return Err(rhs.error_at(
                    "frame starting from current row cannot have preceding rows",
                    rhs.loc(4),
                ));
            }
            if (frame_options & frame_option::START_OFFSET_FOLLOWING) != 0
                && (frame_options & (frame_option::END_OFFSET_PRECEDING | frame_option::END_CURRENT_ROW)) != 0
            {
                return Err(rhs.error_at(
                    "frame starting from following row cannot have preceding rows",
                    rhs.loc(4),
                ));
            }
            n1.frame_options = frame_options;
            n1.end_offset = n2.start_offset;
            Ok(SemValue::node(n1))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// frame_bound:
///       UNBOUNDED PRECEDING
///     | UNBOUNDED FOLLOWING
///     | CURRENT_P ROW
///     | a_expr PRECEDING
///     | a_expr FOLLOWING
/// ```
pub(super) fn frame_bound<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // UNBOUNDED PRECEDING
        0 => {
            let n = WindowDef {
                frame_options: frame_option::START_UNBOUNDED_PRECEDING,
                start_offset: None,
                end_offset: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // UNBOUNDED FOLLOWING
        1 => {
            let n = WindowDef {
                frame_options: frame_option::START_UNBOUNDED_FOLLOWING,
                start_offset: None,
                end_offset: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // CURRENT_P ROW
        2 => {
            let n = WindowDef {
                frame_options: frame_option::START_CURRENT_ROW,
                start_offset: None,
                end_offset: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // a_expr PRECEDING
        3 => {
            let n = WindowDef {
                frame_options: frame_option::START_OFFSET_PRECEDING,
                start_offset: rhs.node(1)?,
                end_offset: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // a_expr FOLLOWING
        4 => {
            let n = WindowDef {
                frame_options: frame_option::START_OFFSET_FOLLOWING,
                start_offset: rhs.node(1)?,
                end_offset: None,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_window_exclusion_clause:
///       EXCLUDE CURRENT_P ROW
///     | EXCLUDE GROUP_P
///     | EXCLUDE TIES
///     | EXCLUDE NO OTHERS
///     | /* empty */
/// ```
pub(super) fn opt_window_exclusion_clause<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // EXCLUDE CURRENT_P ROW
        0 => Ok(SemValue::Int(frame_option::EXCLUDE_CURRENT_ROW)),
        // EXCLUDE GROUP_P
        1 => Ok(SemValue::Int(frame_option::EXCLUDE_GROUP)),
        // EXCLUDE TIES
        2 => Ok(SemValue::Int(frame_option::EXCLUDE_TIES)),
        // EXCLUDE NO OTHERS
        // /* empty */
        3 | 4 => Ok(SemValue::Int(0)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qualified_row:
///       ROW '(' expr_list_opt_comma ')'
///     | ROW '(' ')'
/// ```
pub(super) fn qualified_row<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ROW '(' ')'
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// row:
///       qualified_row
///     | '(' expr_list ',' a_expr ')'
/// ```
pub(super) fn row<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '(' expr_list ',' a_expr ')'
        1 => Ok(SemValue::List(append(rhs.list(2)?, rhs.required_node(4)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// dict_arg:
///       ColIdOrString SINGLE_COLON a_expr
/// ```
pub(super) fn dict_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColIdOrString SINGLE_COLON a_expr
        0 => {
            let na = NamedArgExpr {
                name: rhs.str(1)?,
                arg: rhs.node(3)?,
                argnumber: -1,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(na))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// dict_arguments:
///       dict_arg
///     | dict_arguments ',' dict_arg
/// ```
pub(super) fn dict_arguments<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // dict_arg
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // dict_arguments ',' dict_arg
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// map_arg:
///       a_expr SINGLE_COLON a_expr
/// ```
pub(super) fn map_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr SINGLE_COLON a_expr
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?, rhs.required_node(3)?])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// map_arguments:
///       map_arg
///     | map_arguments ',' map_arg
/// ```
pub(super) fn map_arguments<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // map_arg
        0 => Ok(SemValue::List(vec![Node::List(rhs.list(1)?)])),
        // map_arguments ',' map_arg
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::List(rhs.list(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_map_arguments_opt_comma:
///       map_arguments_opt_comma
///     | /* empty */
/// ```
pub(super) fn opt_map_arguments_opt_comma<'a>(
    rhs: &mut Reduction<'_, 'a>,
    alt: usize,
) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// sub_type:
///       ANY
///     | SOME
///     | ALL
/// ```
pub(super) fn sub_type<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ANY
        // SOME
        0 | 1 => Ok(SemValue::SubLinkType(SubLinkType::Any)),
        // ALL
        2 => Ok(SemValue::SubLinkType(SubLinkType::All)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// all_Op:
///       Op
///     | MathOp
/// ```
pub(super) fn all_op<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // MathOp
        1 => Ok(SemValue::Str(
            rhs.const_str(1)?.map(rustdb_common::ByteString::from),
        )),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// MathOp:
///       '+'
///     | '-'
///     | '*'
///     | '/'
///     | INTEGER_DIVISION
///     | '%'
///     | '^'
///     | POWER_OF
///     | '<'
///     | '>'
///     | '='
///     | LESS_EQUALS
///     | GREATER_EQUALS
///     | NOT_EQUALS
/// ```
pub(super) fn math_op<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '+'
        0 => Ok(SemValue::ConstStr(Some("+"))),
        // '-'
        1 => Ok(SemValue::ConstStr(Some("-"))),
        // '*'
        2 => Ok(SemValue::ConstStr(Some("*"))),
        // '/'
        3 => Ok(SemValue::ConstStr(Some("/"))),
        // INTEGER_DIVISION
        4 => Ok(SemValue::ConstStr(Some("//"))),
        // '%'
        5 => Ok(SemValue::ConstStr(Some("%"))),
        // '^'
        6 => Ok(SemValue::ConstStr(Some("^"))),
        // POWER_OF
        7 => Ok(SemValue::ConstStr(Some("**"))),
        // '<'
        8 => Ok(SemValue::ConstStr(Some("<"))),
        // '>'
        9 => Ok(SemValue::ConstStr(Some(">"))),
        // '='
        10 => Ok(SemValue::ConstStr(Some("="))),
        // LESS_EQUALS
        11 => Ok(SemValue::ConstStr(Some("<="))),
        // GREATER_EQUALS
        12 => Ok(SemValue::ConstStr(Some(">="))),
        // NOT_EQUALS
        13 => Ok(SemValue::ConstStr(Some("<>"))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qual_Op:
///       Op
///     | OPERATOR '(' any_operator ')'
/// ```
pub(super) fn qual_op<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Op
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qual_all_Op:
///       all_Op
///     | OPERATOR '(' any_operator ')'
/// ```
pub(super) fn qual_all_op<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // all_Op
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// subquery_Op:
///       all_Op
///     | OPERATOR '(' any_operator ')'
///     | LIKE
///     | NOT_LA LIKE
///     | GLOB
///     | NOT_LA GLOB
///     | ILIKE
///     | NOT_LA ILIKE
/// ```
pub(super) fn subquery_op<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // all_Op
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        // LIKE
        2 => Ok(SemValue::List(vec![Node::string("~~")])),
        // NOT_LA LIKE
        3 => Ok(SemValue::List(vec![Node::string("!~~")])),
        // GLOB
        4 => Ok(SemValue::List(vec![Node::string("~~~")])),
        // NOT_LA GLOB
        5 => Ok(SemValue::List(vec![Node::string("!~~~")])),
        // ILIKE
        6 => Ok(SemValue::List(vec![Node::string("~~*")])),
        // NOT_LA ILIKE
        7 => Ok(SemValue::List(vec![Node::string("!~~*")])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// any_operator:
///       all_Op
///     | ColId '.' any_operator
/// ```
pub(super) fn any_operator<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // all_Op
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        // ColId '.' any_operator
        1 => Ok(SemValue::List(prepend(
            Node::string(rhs.string(1)?),
            rhs.list(3)?,
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// c_expr_list:
///       c_expr
///     | c_expr_list ',' c_expr
/// ```
pub(super) fn c_expr_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // c_expr
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // c_expr_list ',' c_expr
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// expr_list:
///       a_expr
///     | expr_list ',' a_expr
/// ```
pub(super) fn expr_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // expr_list ',' a_expr
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_expr_list_opt_comma:
///       expr_list_opt_comma
///     | /* empty */
/// ```
pub(super) fn opt_expr_list_opt_comma<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_arg_list:
///       func_arg_expr
///     | func_arg_list ',' func_arg_expr
/// ```
pub(super) fn func_arg_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // func_arg_expr
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // func_arg_list ',' func_arg_expr
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_arg_expr:
///       a_expr
///     | param_name COLON_EQUALS a_expr
///     | param_name EQUALS_GREATER a_expr
/// ```
pub(super) fn func_arg_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // param_name COLON_EQUALS a_expr
        // param_name EQUALS_GREATER a_expr
        1 | 2 => {
            let na = NamedArgExpr {
                name: rhs.str(1)?,
                arg: rhs.node(3)?,
                argnumber: -1,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(na))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// type_list:
///       Typename
///     | type_list ',' Typename
/// ```
pub(super) fn type_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Typename
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // type_list ',' Typename
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// extract_list:
///       extract_arg FROM a_expr
///     | /* empty */
/// ```
pub(super) fn extract_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // extract_arg FROM a_expr
        0 => Ok(SemValue::List(vec![
            make_string_const(rhs.string(1)?, rhs.loc(1)),
            rhs.required_node(3)?,
        ])),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// extract_arg:
///       IDENT
///     | year_keyword
///     | month_keyword
///     | day_keyword
///     | hour_keyword
///     | minute_keyword
///     | second_keyword
///     | millisecond_keyword
///     | microsecond_keyword
///     | week_keyword
///     | quarter_keyword
///     | decade_keyword
///     | century_keyword
///     | millennium_keyword
///     | Sconst
/// ```
pub(super) fn extract_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // year_keyword
        1 => Ok(SemValue::Str(Some("year".into()))),
        // month_keyword
        2 => Ok(SemValue::Str(Some("month".into()))),
        // day_keyword
        3 => Ok(SemValue::Str(Some("day".into()))),
        // hour_keyword
        4 => Ok(SemValue::Str(Some("hour".into()))),
        // minute_keyword
        5 => Ok(SemValue::Str(Some("minute".into()))),
        // second_keyword
        6 => Ok(SemValue::Str(Some("second".into()))),
        // millisecond_keyword
        7 => Ok(SemValue::Str(Some("millisecond".into()))),
        // microsecond_keyword
        8 => Ok(SemValue::Str(Some("microsecond".into()))),
        // week_keyword
        9 => Ok(SemValue::Str(Some("week".into()))),
        // quarter_keyword
        10 => Ok(SemValue::Str(Some("quarter".into()))),
        // decade_keyword
        11 => Ok(SemValue::Str(Some("decade".into()))),
        // century_keyword
        12 => Ok(SemValue::Str(Some("century".into()))),
        // millennium_keyword
        13 => Ok(SemValue::Str(Some("millennium".into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// overlay_list:
///       a_expr overlay_placing substr_from substr_for
///     | a_expr overlay_placing substr_from
/// ```
pub(super) fn overlay_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr overlay_placing substr_from substr_for
        0 => Ok(SemValue::List(vec![
            rhs.required_node(1)?,
            rhs.required_node(2)?,
            rhs.required_node(3)?,
            rhs.required_node(4)?,
        ])),
        // a_expr overlay_placing substr_from
        1 => Ok(SemValue::List(vec![
            rhs.required_node(1)?,
            rhs.required_node(2)?,
            rhs.required_node(3)?,
        ])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// position_list:
///       b_expr IN_P b_expr
///     | /* empty */
/// ```
pub(super) fn position_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // b_expr IN_P b_expr
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?, rhs.required_node(3)?])),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// substr_list:
///       a_expr substr_from substr_for
///     | a_expr substr_for substr_from
///     | a_expr substr_from
///     | a_expr substr_for
///     | expr_list
///     | /* empty */
/// ```
pub(super) fn substr_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr substr_from substr_for
        0 => Ok(SemValue::List(vec![
            rhs.required_node(1)?,
            rhs.required_node(2)?,
            rhs.required_node(3)?,
        ])),
        // a_expr substr_for substr_from
        1 => Ok(SemValue::List(vec![
            rhs.required_node(1)?,
            rhs.required_node(3)?,
            rhs.required_node(2)?,
        ])),
        // a_expr substr_from
        2 => Ok(SemValue::List(vec![rhs.required_node(1)?, rhs.required_node(2)?])),
        // a_expr substr_for
        3 => Ok(SemValue::List(vec![
            rhs.required_node(1)?,
            make_int_const(1, -1),
            make_type_cast(rhs.node(2)?, Some(Box::new(system_type_name("int4"))), 0, -1),
        ])),
        // /* empty */
        5 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// trim_list:
///       a_expr FROM expr_list_opt_comma
///     | FROM expr_list_opt_comma
///     | expr_list_opt_comma
/// ```
pub(super) fn trim_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr FROM expr_list_opt_comma
        0 => Ok(SemValue::List(append(rhs.list(3)?, rhs.required_node(1)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// in_expr:
///       select_with_parens
///     | '(' expr_list_opt_comma ')'
///     | columnref_opt_indirection
///     | indirection_expr
/// ```
pub(super) fn in_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // select_with_parens
        0 => {
            let n = SubLink {
                subselect: rhs.node(1)?,
                ..Default::default()
            };
            Ok(SemValue::node(n))
        }
        // '(' expr_list_opt_comma ')'
        1 => Ok(SemValue::Node(Node::from_list(rhs.list(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// case_expr:
///       CASE case_arg when_clause_list case_default END_P
/// ```
pub(super) fn case_expr<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CASE case_arg when_clause_list case_default END_P
        0 => {
            let c = CaseExpr {
                arg: rhs.node(2)?,
                args: rhs.list(3)?,
                defresult: rhs.node(4)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(c))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// when_clause_list:
///       when_clause
///     | when_clause_list when_clause
/// ```
pub(super) fn when_clause_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // when_clause
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // when_clause_list when_clause
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// when_clause:
///       WHEN a_expr THEN a_expr
/// ```
pub(super) fn when_clause<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WHEN a_expr THEN a_expr
        0 => {
            let w = CaseWhen {
                expr: rhs.node(2)?,
                result: rhs.node(4)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(w))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// case_default:
///       ELSE a_expr
///     | /* empty */
/// ```
pub(super) fn case_default<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// case_arg:
///       a_expr
///     | /* empty */
/// ```
pub(super) fn case_arg<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// columnref:
///       ColId
/// ```
pub(super) fn columnref<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::node(make_column_ref(
            rhs.string(1)?,
            List::new(),
            rhs.loc(1),
            rhs.scanner(),
        )?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// columnref_opt_indirection:
///       ColId
///     | ColId indirection
/// ```
pub(super) fn columnref_opt_indirection<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::node(make_column_ref(
            rhs.string(1)?,
            List::new(),
            rhs.loc(1),
            rhs.scanner(),
        )?)),
        // ColId indirection
        1 => Ok(SemValue::node(make_column_ref(
            rhs.string(1)?,
            rhs.list(2)?,
            rhs.loc(1),
            rhs.scanner(),
        )?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_slice_bound:
///       a_expr
///     | /* empty */
/// ```
pub(super) fn opt_slice_bound<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_indirection:
///       /* empty */
///     | opt_indirection indirection_el
/// ```
pub(super) fn opt_indirection<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::List(List::new())),
        // opt_indirection indirection_el
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_func_arguments:
///       /* empty */
///     | '(' ')'
///     | '(' func_arg_list ')'
/// ```
pub(super) fn opt_func_arguments<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::OptList(None)),
        // '(' ')'
        1 => Ok(SemValue::OptList(Some(List::new()))),
        // '(' func_arg_list ')'
        2 => Ok(SemValue::OptList(Some(rhs.list(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// extended_indirection_el:
///       '.' attr_name opt_func_arguments
///     | '[' a_expr ']'
///     | '[' opt_slice_bound SINGLE_COLON opt_slice_bound ']'
///     | '[' opt_slice_bound SINGLE_COLON opt_slice_bound SINGLE_COLON opt_slice_bound ']'
///     | '[' opt_slice_bound SINGLE_COLON '-' SINGLE_COLON opt_slice_bound ']'
/// ```
pub(super) fn extended_indirection_el<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '.' attr_name opt_func_arguments
        0 => {
            let name = rhs.string(2)?;
            let node = match rhs.opt_list(3)? {
                Some(args) => make_func_call(vec![Node::string(name)], args, rhs.loc(2)).into(),
                None => Node::string(name),
            };
            Ok(SemValue::node(node))
        }
        // '[' a_expr ']'
        1 => {
            let ai = AIndices {
                is_slice: false,
                lidx: None,
                uidx: rhs.node(2)?,
                ..Default::default()
            };
            Ok(SemValue::node(ai))
        }
        // '[' opt_slice_bound SINGLE_COLON opt_slice_bound ']'
        2 => {
            let ai = AIndices {
                is_slice: true,
                lidx: rhs.node(2)?,
                uidx: rhs.node(4)?,
                ..Default::default()
            };
            Ok(SemValue::node(ai))
        }
        // '[' opt_slice_bound SINGLE_COLON opt_slice_bound SINGLE_COLON opt_slice_bound ']'
        3 => {
            let ai = AIndices {
                is_slice: true,
                lidx: rhs.node(2)?,
                uidx: rhs.node(4)?,
                step: rhs.node(6)?,
            };
            Ok(SemValue::node(ai))
        }
        // '[' opt_slice_bound SINGLE_COLON '-' SINGLE_COLON opt_slice_bound ']'
        4 => {
            let ai = AIndices {
                is_slice: true,
                lidx: rhs.node(2)?,
                step: rhs.node(6)?,
                ..Default::default()
            };
            Ok(SemValue::node(ai))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_extended_indirection:
///       /* empty */
///     | opt_extended_indirection extended_indirection_el
/// ```
pub(super) fn opt_extended_indirection<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::List(List::new())),
        // opt_extended_indirection extended_indirection_el
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_target_list_opt_comma:
///       target_list_opt_comma
///     | /* empty */
/// ```
pub(super) fn opt_target_list_opt_comma<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// target_list:
///       target_el
///     | target_list ',' target_el
/// ```
pub(super) fn target_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // target_el
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // target_list ',' target_el
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// target_el:
///       a_expr AS ColLabelOrString
///     | a_expr IDENT
///     | a_expr
///     | ColId SINGLE_COLON a_expr
/// ```
pub(super) fn target_el<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr AS ColLabelOrString
        0 => {
            let target_el = ResTarget {
                name: rhs.str(3)?,
                indirection: List::new(),
                val: rhs.node(1)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(target_el))
        }
        // a_expr IDENT
        1 => {
            let target_el = ResTarget {
                name: rhs.str(2)?,
                indirection: List::new(),
                val: rhs.node(1)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(target_el))
        }
        // a_expr
        2 => {
            let target_el = ResTarget {
                name: None,
                indirection: List::new(),
                val: rhs.node(1)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(target_el))
        }
        // ColId SINGLE_COLON a_expr
        3 => {
            let target_el = ResTarget {
                name: rhs.str(1)?,
                indirection: List::new(),
                val: rhs.node(3)?,
                location: rhs.loc(1),
            };
            Ok(SemValue::node(target_el))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// except_list:
///       EXCLUDE '(' except_name_list_opt_comma ')'
///     | EXCLUDE except_name
/// ```
pub(super) fn except_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // EXCLUDE except_name
        1 => Ok(SemValue::List(vec![Node::List(rhs.list(2)?)])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// except_name:
///       ColIdOrString
///     | except_name '.' ColIdOrString
/// ```
pub(super) fn except_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColIdOrString
        0 => Ok(SemValue::List(vec![Node::Str(rhs.string(1)?)])),
        // except_name '.' ColIdOrString
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::Str(rhs.string(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// except_name_list:
///       except_name
///     | except_name_list ',' except_name
/// ```
pub(super) fn except_name_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // except_name
        0 => Ok(SemValue::List(vec![Node::List(rhs.list(1)?)])),
        // except_name_list ',' except_name
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::List(rhs.list(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_except_list:
///       except_list
///     | /* empty */
/// ```
pub(super) fn opt_except_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// replace_list_el:
///       a_expr AS ColId
/// ```
pub(super) fn replace_list_el<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // a_expr AS ColId
        0 => Ok(SemValue::List(vec![
            rhs.required_node(1)?,
            Node::string(rhs.string(3)?),
        ])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// replace_list:
///       replace_list_el
///     | replace_list ',' replace_list_el
/// ```
pub(super) fn replace_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // replace_list_el
        0 => Ok(SemValue::List(vec![Node::List(rhs.list(1)?)])),
        // replace_list ',' replace_list_el
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::List(rhs.list(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_replace_list:
///       REPLACE '(' replace_list_opt_comma ')'
///     | REPLACE replace_list_el
///     | /* empty */
/// ```
pub(super) fn opt_replace_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // REPLACE replace_list_el
        1 => Ok(SemValue::List(vec![Node::List(rhs.list(2)?)])),
        // /* empty */
        2 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// rename_list_el:
///       except_name AS ColId
/// ```
pub(super) fn rename_list_el<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // except_name AS ColId
        0 => Ok(SemValue::List(vec![
            Node::List(rhs.list(1)?),
            Node::Str(rhs.string(3)?),
        ])),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// rename_list:
///       rename_list_el
///     | rename_list ',' rename_list_el
/// ```
pub(super) fn rename_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // rename_list_el
        0 => Ok(SemValue::List(vec![Node::List(rhs.list(1)?)])),
        // rename_list ',' rename_list_el
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::List(rhs.list(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_rename_list:
///       RENAME '(' rename_list_opt_comma ')'
///     | RENAME rename_list_el
///     | /* empty */
/// ```
pub(super) fn opt_rename_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // RENAME rename_list_el
        1 => Ok(SemValue::List(vec![Node::List(rhs.list(2)?)])),
        // /* empty */
        2 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// qualified_name_list:
///       qualified_name
///     | qualified_name_list ',' qualified_name
/// ```
pub(super) fn qualified_name_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // qualified_name
        0 => Ok(SemValue::List(vec![rhs.required_node(1)?])),
        // qualified_name_list ',' qualified_name
        1 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(3)?))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// name_list:
///       name
///     | name_list ',' name
/// ```
pub(super) fn name_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // name
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        // name_list ',' name
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::string(rhs.string(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// func_name:
///       function_name_token
///     | ColId indirection
/// ```
pub(super) fn func_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // function_name_token
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        // ColId indirection
        1 => Ok(SemValue::List(check_func_name(
            prepend(Node::string(rhs.string(1)?), rhs.list(2)?),
            rhs.scanner(),
        )?)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// AexprConst:
///       Iconst
///     | FCONST
///     | Sconst opt_indirection
///     | BCONST
///     | XCONST
///     | func_name Sconst
///     | func_name '(' func_arg_list opt_sort_clause opt_ignore_nulls ')' Sconst
///     | ConstTypename Sconst
///     | ConstInterval '(' a_expr ')' opt_interval
///     | ConstInterval Iconst opt_interval
///     | ConstInterval Sconst opt_interval
///     | TRUE_P
///     | FALSE_P
///     | NULL_P
/// ```
pub(super) fn aexpr_const<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Iconst
        0 => Ok(SemValue::node(make_int_const(rhs.int(1)?, rhs.loc(1)))),
        // FCONST
        1 => Ok(SemValue::node(make_float_const(rhs.string(1)?, rhs.loc(1)))),
        // Sconst opt_indirection
        2 => {
            let constant = make_string_const(rhs.string(1)?, rhs.loc(1));
            let indirection = rhs.list(2)?;
            if indirection.is_empty() {
                return Ok(SemValue::node(constant));
            }
            let indirection = check_indirection(indirection, rhs.scanner())?;
            Ok(SemValue::node(AIndirection {
                arg: Some(constant),
                indirection,
            }))
        }
        // BCONST
        // XCONST
        3 | 4 => Ok(SemValue::node(make_bit_string_const(rhs.string(1)?, rhs.loc(1)))),
        // func_name Sconst
        5 => {
            let mut t = Some(Box::new(make_type_name_from_name_list(rhs.list(1)?)));
            required_mut(&mut t)?.location = rhs.loc(1);
            Ok(SemValue::node(make_string_const_cast(
                rhs.string(2)?,
                rhs.loc(2),
                t,
            )))
        }
        // func_name '(' func_arg_list opt_sort_clause opt_ignore_nulls ')' Sconst
        6 => {
            // generic syntax with a type modifier
            let mut type_name = make_type_name_from_name_list(rhs.list(1)?);
            let args = rhs.list(3)?;
            // func_arg_list and opt_sort_clause avoid grammar conflicts, but parameter names,
            // ORDER BY and IGNORE NULLS are not allowed here
            if let Some(Node::NamedArgExpr(arg)) =
                args.iter().find(|arg| matches!(arg, Node::NamedArgExpr(_)))
            {
                return Err(rhs.error_at("type modifier cannot have parameter name", arg.location));
            }
            if !rhs.list(4)?.is_empty() {
                return Err(rhs.error_at("type modifier cannot have ORDER BY", rhs.loc(4)));
            }
            if rhs.ignore_nulls(5)? != IgnoreNulls::Default {
                return Err(rhs.error_at("type modifier cannot have IGNORE NULLS", rhs.loc(5)));
            }
            type_name.typmods = args;
            type_name.location = rhs.loc(1);
            Ok(SemValue::node(make_string_const_cast(
                rhs.string(7)?,
                rhs.loc(7),
                Some(Box::new(type_name)),
            )))
        }
        // ConstTypename Sconst
        7 => Ok(SemValue::node(make_string_const_cast(
            rhs.string(2)?,
            rhs.loc(2),
            rhs.ptr::<TypeName>(1)?,
        ))),
        // ConstInterval '(' a_expr ')' opt_interval
        8 => Ok(SemValue::node(make_interval_node(
            IntervalValue::Expr(rhs.node(3)?),
            rhs.loc(3),
            rhs.list(5)?,
        ))),
        // ConstInterval Iconst opt_interval
        9 => Ok(SemValue::node(make_interval_node(
            IntervalValue::Integer(rhs.int(2)?),
            rhs.loc(2),
            rhs.list(3)?,
        ))),
        // ConstInterval Sconst opt_interval
        10 => Ok(SemValue::node(make_interval_node(
            IntervalValue::String(rhs.string(2)?),
            rhs.loc(2),
            rhs.list(3)?,
        ))),
        // TRUE_P
        11 => Ok(SemValue::node(make_bool_a_const(true, rhs.loc(1)))),
        // FALSE_P
        12 => Ok(SemValue::node(make_bool_a_const(false, rhs.loc(1)))),
        // NULL_P
        13 => Ok(SemValue::node(make_null_a_const(rhs.loc(1)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// type_function_name:
///       IDENT
///     | unreserved_keyword
///     | type_func_name_keyword
/// ```
pub(super) fn type_function_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // unreserved_keyword
        // type_func_name_keyword
        1 | 2 => Ok(SemValue::Str(Some(rhs.keyword(1)?.into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// function_name_token:
///       IDENT
///     | unreserved_keyword
///     | func_name_keyword
/// ```
pub(super) fn function_name_token<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // unreserved_keyword
        // func_name_keyword
        1 | 2 => Ok(SemValue::Str(Some(rhs.keyword(1)?.into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// type_name_token:
///       IDENT
///     | unreserved_keyword
///     | type_name_keyword
/// ```
pub(super) fn type_name_token<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // unreserved_keyword
        // type_name_keyword
        1 | 2 => Ok(SemValue::Str(Some(rhs.keyword(1)?.into()))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// any_name:
///       ColId
///     | ColId attrs
/// ```
pub(super) fn any_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // ColId
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(1)?)])),
        // ColId attrs
        1 => Ok(SemValue::List(prepend(
            Node::string(rhs.string(1)?),
            rhs.list(2)?,
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// attrs:
///       '.' attr_name
///     | attrs '.' attr_name
/// ```
pub(super) fn attrs<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // '.' attr_name
        0 => Ok(SemValue::List(vec![Node::string(rhs.string(2)?)])),
        // attrs '.' attr_name
        1 => Ok(SemValue::List(append(rhs.list(1)?, Node::string(rhs.string(3)?)))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_name_list:
///       '(' name_list_opt_comma ')'
///     | /* empty */
/// ```
pub(super) fn opt_name_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}
