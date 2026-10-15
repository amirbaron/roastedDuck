//! DML statements (INSERT, UPDATE, DELETE, MERGE INTO, COPY).

use rustdb_ast::SqlStatementKind;
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{CopyStmt, DeleteStmt, InsertStmt, MergeIntoStmt, UpdateStmt};

use super::Transformer;

fn pending(what: &str) -> Error {
    Error::not_implemented(format!("{what} is not supported by the parser yet"))
}

impl<'a> Transformer<'a> {
    pub(crate) fn transform_insert(&mut self, _stmt: &'a InsertStmt) -> Result<SqlStatementKind> {
        Err(pending("INSERT"))
    }

    pub(crate) fn transform_update(&mut self, _stmt: &'a UpdateStmt) -> Result<SqlStatementKind> {
        Err(pending("UPDATE"))
    }

    pub(crate) fn transform_delete(&mut self, _stmt: &'a DeleteStmt) -> Result<SqlStatementKind> {
        Err(pending("DELETE"))
    }

    pub(crate) fn transform_merge_into(&mut self, _stmt: &'a MergeIntoStmt) -> Result<SqlStatementKind> {
        Err(pending("MERGE INTO"))
    }

    pub(crate) fn transform_copy(&mut self, _stmt: &'a CopyStmt) -> Result<SqlStatementKind> {
        Err(pending("COPY"))
    }
}
