//! The other statements (SET, PRAGMA, PREPARE/EXECUTE, EXPLAIN, ...).

use rustdb_ast::SqlStatementKind;
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{
    AttachStmt, CallStmt, CheckPointStmt, CopyDatabaseStmt, DeallocateStmt, DetachStmt, ExecuteStmt,
    ExplainStmt, ExportStmt, ImportStmt, LoadStmt, PragmaStmt, PrepareStmt, TransactionStmt,
    UpdateExtensionsStmt, UseStmt, VacuumStmt, VariableSetStmt,
};

use super::Transformer;

fn pending(what: &str) -> Error {
    Error::not_implemented(format!("{what} is not supported by the parser yet"))
}

impl<'a> Transformer<'a> {
    pub(crate) fn transform_transaction(&mut self, _stmt: &'a TransactionStmt) -> Result<SqlStatementKind> {
        Err(pending("Transactions"))
    }

    pub(crate) fn transform_update_extensions(
        &mut self,
        _stmt: &'a UpdateExtensionsStmt,
    ) -> SqlStatementKind {
        SqlStatementKind::Multi(Vec::new())
    }

    pub(crate) fn transform_prepare(&mut self, _stmt: &'a PrepareStmt) -> Result<SqlStatementKind> {
        Err(pending("PREPARE"))
    }

    pub(crate) fn transform_execute(&mut self, _stmt: &'a ExecuteStmt) -> Result<SqlStatementKind> {
        Err(pending("EXECUTE"))
    }

    pub(crate) fn transform_deallocate(&mut self, _stmt: &'a DeallocateStmt) -> Result<SqlStatementKind> {
        Err(pending("DEALLOCATE"))
    }

    pub(crate) fn transform_pragma(&mut self, _stmt: &'a PragmaStmt) -> Result<SqlStatementKind> {
        Err(pending("PRAGMA"))
    }

    pub(crate) fn transform_export(&mut self, _stmt: &'a ExportStmt) -> Result<SqlStatementKind> {
        Err(pending("EXPORT"))
    }

    pub(crate) fn transform_import(&mut self, _stmt: &'a ImportStmt) -> SqlStatementKind {
        SqlStatementKind::Multi(Vec::new())
    }

    pub(crate) fn transform_explain(&mut self, _stmt: &'a ExplainStmt) -> Result<SqlStatementKind> {
        Err(pending("EXPLAIN"))
    }

    pub(crate) fn transform_vacuum(&mut self, _stmt: &'a VacuumStmt) -> Result<SqlStatementKind> {
        Err(pending("VACUUM"))
    }

    pub(crate) fn transform_call(&mut self, _stmt: &'a CallStmt) -> Result<SqlStatementKind> {
        Err(pending("CALL"))
    }

    pub(crate) fn transform_set(&mut self, _stmt: &'a VariableSetStmt) -> Result<SqlStatementKind> {
        Err(pending("SET"))
    }

    pub(crate) fn transform_checkpoint(&mut self, _stmt: &'a CheckPointStmt) -> SqlStatementKind {
        SqlStatementKind::Multi(Vec::new())
    }

    pub(crate) fn transform_load(&mut self, _stmt: &'a LoadStmt) -> SqlStatementKind {
        SqlStatementKind::Multi(Vec::new())
    }

    pub(crate) fn transform_attach(&mut self, _stmt: &'a AttachStmt) -> Result<SqlStatementKind> {
        Err(pending("ATTACH"))
    }

    pub(crate) fn transform_detach(&mut self, _stmt: &'a DetachStmt) -> SqlStatementKind {
        SqlStatementKind::Multi(Vec::new())
    }

    pub(crate) fn transform_use(&mut self, _stmt: &'a UseStmt) -> Result<SqlStatementKind> {
        Err(pending("USE"))
    }

    pub(crate) fn transform_copy_database(
        &mut self,
        _stmt: &'a CopyDatabaseStmt,
    ) -> Result<SqlStatementKind> {
        Err(pending("COPY FROM DATABASE"))
    }
}
