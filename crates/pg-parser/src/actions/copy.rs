//! Semantic actions for the rules in `grammar/statements/copy.y`.

use super::prelude::*;

/// ```text
/// CopyStmt:
///       COPY opt_binary qualified_name opt_column_list opt_oids copy_from opt_program copy_file_name copy_delimiter opt_with copy_options
///     | COPY '(' SelectStmt ')' TO opt_program copy_file_name opt_with copy_options
///     | COPY FROM DATABASE ColId TO ColId copy_database_flag
/// ```
pub(super) fn copy_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // COPY opt_binary qualified_name opt_column_list opt_oids copy_from opt_program copy_file_name copy_delimiter opt_with copy_options
        0 => {
            let opt_binary = rhs.ptr::<DefElem>(2)?;
            let opt_oids = rhs.ptr::<DefElem>(5)?;
            let copy_delimiter = rhs.ptr::<DefElem>(9)?;
            let copy_options = rhs.list(11)?;
            let mut n = CopyStmt {
                relation: rhs.ptr::<RangeVar>(3)?,
                query: None,
                attlist: rhs.list(4)?,
                is_from: rhs.bool(6)?,
                is_program: rhs.bool(7)?,
                filename: rhs.node(8)?,
                ..Default::default()
            };
            if n.is_program && n.filename.is_none() {
                return Err(rhs.error_at("STDIN/STDOUT not allowed with PROGRAM", rhs.loc(8)));
            }
            // concatenate user-supplied flags
            let flags = [opt_binary, opt_oids, copy_delimiter].into_iter().flatten();
            n.options = flags.map(Node::from).chain(copy_options).collect();
            Ok(SemValue::node(n))
        }
        // COPY '(' SelectStmt ')' TO opt_program copy_file_name opt_with copy_options
        1 => {
            let n = CopyStmt {
                relation: None,
                query: rhs.node(3)?,
                attlist: List::new(),
                is_from: false,
                is_program: rhs.bool(6)?,
                filename: rhs.node(7)?,
                options: rhs.list(9)?,
            };
            if n.is_program && n.filename.is_none() {
                return Err(rhs.error_at("STDIN/STDOUT not allowed with PROGRAM", rhs.loc(5)));
            }
            Ok(SemValue::node(n))
        }
        // COPY FROM DATABASE ColId TO ColId copy_database_flag
        2 => {
            let n = CopyDatabaseStmt {
                from_database: rhs.str(4)?,
                to_database: rhs.str(6)?,
                copy_database_flag: rhs.const_str(7)?.map(str::to_owned),
            };
            Ok(SemValue::node(n))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// copy_database_flag:
///       /* empty */
///     | '(' SCHEMA ')'
///     | '(' DATA_P ')'
/// ```
pub(super) fn copy_database_flag<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // /* empty */
        0 => Ok(SemValue::ConstStr(None)),
        // '(' SCHEMA ')'
        1 => Ok(SemValue::ConstStr(Some("schema"))),
        // '(' DATA_P ')'
        2 => Ok(SemValue::ConstStr(Some("data"))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// copy_from:
///       FROM
///     | TO
/// ```
pub(super) fn copy_from<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // FROM
        0 => Ok(SemValue::Bool(true)),
        // TO
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// copy_delimiter:
///       opt_using DELIMITERS Sconst
///     | /* empty */
/// ```
pub(super) fn copy_delimiter<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // opt_using DELIMITERS Sconst
        0 => Ok(SemValue::node(make_def_elem(
            "delimiter",
            Some(Node::string(rhs.string(3)?)),
            rhs.loc(2),
        ))),
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_program:
///       PROGRAM
///     | /* empty */
/// ```
pub(super) fn opt_program<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // PROGRAM
        0 => Ok(SemValue::Bool(true)),
        // /* empty */
        1 => Ok(SemValue::Bool(false)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_oids:
///       WITH OIDS
///     | /* empty */
/// ```
pub(super) fn opt_oids<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH OIDS
        0 => Ok(SemValue::node(make_def_elem("oids", None, rhs.loc(1)))),
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// copy_opt_list:
///       copy_opt_list copy_opt_item
///     | /* empty */
/// ```
pub(super) fn copy_opt_list<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // copy_opt_list copy_opt_item
        0 => Ok(SemValue::List(append(rhs.list(1)?, rhs.required_node(2)?))),
        // /* empty */
        1 => Ok(SemValue::List(List::new())),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_binary:
///       BINARY
///     | /* empty */
/// ```
pub(super) fn opt_binary<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BINARY
        0 => Ok(SemValue::node(make_def_elem(
            "format",
            Some(make_string_const("binary", rhs.loc(1))),
            rhs.loc(1),
        ))),
        // /* empty */
        1 => Ok(SemValue::Node(None)),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// copy_opt_item:
///       BINARY
///     | OIDS
///     | FREEZE
///     | DELIMITER opt_as Sconst
///     | NULL_P opt_as Sconst
///     | CSV
///     | HEADER_P
///     | QUOTE opt_as Sconst
///     | ESCAPE opt_as Sconst
///     | FORCE QUOTE columnList
///     | FORCE QUOTE '*'
///     | PARTITION BY columnList
///     | PARTITION BY '*'
///     | FORCE NOT NULL_P columnList
///     | FORCE NULL_P columnList
///     | ENCODING Sconst
/// ```
pub(super) fn copy_opt_item<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // BINARY
        0 => Ok(SemValue::node(make_def_elem(
            "format",
            Some(make_string_const("binary", rhs.loc(1))),
            rhs.loc(1),
        ))),
        // OIDS
        1 => Ok(SemValue::node(make_def_elem("oids", None, rhs.loc(1)))),
        // FREEZE
        2 => Ok(SemValue::node(make_def_elem("freeze", None, rhs.loc(1)))),
        // DELIMITER opt_as Sconst
        3 => Ok(SemValue::node(make_def_elem(
            "delimiter",
            Some(make_string_const(rhs.string(3)?, rhs.loc(3))),
            rhs.loc(1),
        ))),
        // NULL_P opt_as Sconst
        4 => Ok(SemValue::node(make_def_elem(
            "null",
            Some(make_string_const(rhs.string(3)?, rhs.loc(3))),
            rhs.loc(1),
        ))),
        // CSV
        5 => Ok(SemValue::node(make_def_elem(
            "format",
            Some(make_string_const("csv", rhs.loc(1))),
            rhs.loc(1),
        ))),
        // HEADER_P
        6 => Ok(SemValue::node(make_def_elem("header", None, rhs.loc(1)))),
        // QUOTE opt_as Sconst
        7 => Ok(SemValue::node(make_def_elem(
            "quote",
            Some(make_string_const(rhs.string(3)?, rhs.loc(3))),
            rhs.loc(1),
        ))),
        // ESCAPE opt_as Sconst
        8 => Ok(SemValue::node(make_def_elem(
            "escape",
            Some(make_string_const(rhs.string(3)?, rhs.loc(3))),
            rhs.loc(1),
        ))),
        // FORCE QUOTE columnList
        9 => Ok(SemValue::node(make_def_elem(
            "force_quote",
            Node::from_list(rhs.list(3)?),
            rhs.loc(1),
        ))),
        // FORCE QUOTE '*'
        10 => Ok(SemValue::node(make_def_elem(
            "force_quote",
            Some(AStar::default().into()),
            rhs.loc(1),
        ))),
        // PARTITION BY columnList
        11 => Ok(SemValue::node(make_def_elem(
            "partition_by",
            Node::from_list(rhs.list(3)?),
            rhs.loc(1),
        ))),
        // PARTITION BY '*'
        12 => Ok(SemValue::node(make_def_elem(
            "partition_by",
            Some(AStar::default().into()),
            rhs.loc(1),
        ))),
        // FORCE NOT NULL_P columnList
        13 => Ok(SemValue::node(make_def_elem(
            "force_not_null",
            Node::from_list(rhs.list(4)?),
            rhs.loc(1),
        ))),
        // FORCE NULL_P columnList
        14 => Ok(SemValue::node(make_def_elem(
            "force_null",
            Node::from_list(rhs.list(3)?),
            rhs.loc(1),
        ))),
        // ENCODING Sconst
        15 => Ok(SemValue::node(make_def_elem(
            "encoding",
            Some(make_string_const(rhs.string(2)?, rhs.loc(2))),
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// copy_file_name:
///       Sconst
///     | STDIN
///     | STDOUT
///     | IDENT '.' ColId
///     | IDENT
///     | '(' a_expr ')'
///     | param_expr
/// ```
pub(super) fn copy_file_name<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // Sconst
        // IDENT
        0 | 4 => Ok(SemValue::node(make_string_const(rhs.string(1)?, rhs.loc(1)))),
        // STDIN
        1 => Ok(SemValue::node(make_string_const("/dev/stdin", rhs.loc(1)))),
        // STDOUT
        2 => Ok(SemValue::node(make_string_const("/dev/stdout", rhs.loc(1)))),
        // IDENT '.' ColId
        3 => Ok(SemValue::node(make_string_const(
            format!(
                "{}.{}",
                c_str(rhs.str(1)?.as_deref()),
                c_str(rhs.str(3)?.as_deref())
            ),
            rhs.loc(1),
        ))),
        _ => Err(rhs.bad_alternative()),
    }
}
