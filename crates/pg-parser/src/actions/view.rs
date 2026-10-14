//! Semantic actions for the rules in `grammar/statements/view.y`.

use super::prelude::*;

/// ```text
/// ViewStmt:
///       CREATE_P OptTemp VIEW qualified_name opt_column_list opt_reloptions AS SelectStmt opt_check_option
///     | CREATE_P OptTemp VIEW IF_P NOT EXISTS qualified_name opt_column_list opt_reloptions AS SelectStmt opt_check_option
///     | CREATE_P OR REPLACE OptTemp VIEW qualified_name opt_column_list opt_reloptions AS SelectStmt opt_check_option
///     | CREATE_P OptTemp RECURSIVE VIEW qualified_name '(' columnList ')' opt_reloptions AS SelectStmt opt_check_option
///     | CREATE_P OR REPLACE OptTemp RECURSIVE VIEW qualified_name '(' columnList ')' opt_reloptions AS SelectStmt opt_check_option
/// ```
pub(super) fn view_stmt<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // CREATE_P OptTemp VIEW qualified_name opt_column_list opt_reloptions AS SelectStmt opt_check_option
        0 => {
            let mut n = ViewStmt {
                view: rhs.ptr::<RangeVar>(4)?,
                ..Default::default()
            };
            required_mut(&mut n.view)?.relpersistence = rhs.rel_persistence(2)?;
            n.aliases = rhs.list(5)?;
            n.query = rhs.node(8)?;
            n.onconflict = OnCreateConflict::Error;
            n.options = rhs.list(6)?;
            n.with_check_option = rhs.view_check_option(9)?;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OptTemp VIEW IF_P NOT EXISTS qualified_name opt_column_list opt_reloptions AS SelectStmt opt_check_option
        1 => {
            let mut n = ViewStmt {
                view: rhs.ptr::<RangeVar>(7)?,
                ..Default::default()
            };
            required_mut(&mut n.view)?.relpersistence = rhs.rel_persistence(2)?;
            n.aliases = rhs.list(8)?;
            n.query = rhs.node(11)?;
            n.onconflict = OnCreateConflict::Ignore;
            n.options = rhs.list(9)?;
            n.with_check_option = rhs.view_check_option(12)?;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OR REPLACE OptTemp VIEW qualified_name opt_column_list opt_reloptions AS SelectStmt opt_check_option
        2 => {
            let mut n = ViewStmt {
                view: rhs.ptr::<RangeVar>(6)?,
                ..Default::default()
            };
            required_mut(&mut n.view)?.relpersistence = rhs.rel_persistence(4)?;
            n.aliases = rhs.list(7)?;
            n.query = rhs.node(10)?;
            n.onconflict = OnCreateConflict::Replace;
            n.options = rhs.list(8)?;
            n.with_check_option = rhs.view_check_option(11)?;
            Ok(SemValue::node(Node::from(n)))
        }
        // CREATE_P OptTemp RECURSIVE VIEW qualified_name '(' columnList ')' opt_reloptions AS SelectStmt opt_check_option
        3 => {
            let mut view = rhs.take::<RangeVar>(5)?;
            view.relpersistence = rhs.rel_persistence(2)?;
            let aliases = rhs.list(7)?;
            // the view's name and column list are also used by the rewritten query
            let query =
                make_recursive_view_select(required(view.relname.clone())?, aliases.clone(), rhs.node(11)?)?;
            let with_check_option = rhs.view_check_option(12)?;
            if with_check_option != ViewCheckOption::None {
                return Err(rhs.error_at("WITH CHECK OPTION not supported on recursive views", rhs.loc(12)));
            }
            Ok(SemValue::node(ViewStmt {
                view: Some(view),
                aliases,
                query: Some(query),
                onconflict: OnCreateConflict::Error,
                options: rhs.list(9)?,
                with_check_option,
            }))
        }
        // CREATE_P OR REPLACE OptTemp RECURSIVE VIEW qualified_name '(' columnList ')' opt_reloptions AS SelectStmt opt_check_option
        4 => {
            let mut view = rhs.take::<RangeVar>(7)?;
            view.relpersistence = rhs.rel_persistence(4)?;
            let aliases = rhs.list(9)?;
            let query =
                make_recursive_view_select(required(view.relname.clone())?, aliases.clone(), rhs.node(13)?)?;
            let with_check_option = rhs.view_check_option(14)?;
            if with_check_option != ViewCheckOption::None {
                return Err(rhs.error_at("WITH CHECK OPTION not supported on recursive views", rhs.loc(14)));
            }
            Ok(SemValue::node(ViewStmt {
                view: Some(view),
                aliases,
                query: Some(query),
                onconflict: OnCreateConflict::Replace,
                options: rhs.list(11)?,
                with_check_option,
            }))
        }
        _ => Err(rhs.bad_alternative()),
    }
}

/// ```text
/// opt_check_option:
///       WITH CHECK_P OPTION
///     | WITH CASCADED CHECK_P OPTION
///     | WITH LOCAL CHECK_P OPTION
///     | /* empty */
/// ```
pub(super) fn opt_check_option<'a>(rhs: &mut Reduction<'_, 'a>, alt: usize) -> Result<SemValue<'a>> {
    match alt {
        // WITH CHECK_P OPTION
        0 => Ok(SemValue::ViewCheckOption(ViewCheckOption::Cascaded)),
        // WITH CASCADED CHECK_P OPTION
        1 => Ok(SemValue::ViewCheckOption(ViewCheckOption::Cascaded)),
        // WITH LOCAL CHECK_P OPTION
        2 => Ok(SemValue::ViewCheckOption(ViewCheckOption::Local)),
        // /* empty */
        3 => Ok(SemValue::ViewCheckOption(ViewCheckOption::None)),
        _ => Err(rhs.bad_alternative()),
    }
}
