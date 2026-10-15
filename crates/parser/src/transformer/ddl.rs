//! DDL statements (CREATE, DROP, ALTER, COMMENT ON).

use rustdb_ast::{SelectStatement, SqlStatementKind};
use rustdb_common::{Error, Result};
use rustdb_pg_parser::nodes::{
    AlterDatabaseStmt, AlterSeqStmt, AlterTableStmt, CommentOnStmt, CreateFunctionStmt, CreateSchemaStmt,
    CreateSecretStmt, CreateSeqStmt, CreateStmt, CreateTableAsStmt, CreateTypeStmt, DropSecretStmt, DropStmt,
    IndexStmt, RenameStmt, ViewStmt,
};

use super::Transformer;

fn pending(what: &str) -> Error {
    Error::not_implemented(format!("{what} is not supported by the parser yet"))
}

/// `GenerateCreateEnumStmt`'s `CREATE TYPE name AS ENUM (query)`.
pub(crate) fn create_enum_type_from_query(_name: String, _query: SelectStatement) -> SqlStatementKind {
    SqlStatementKind::Multi(Vec::new())
}

impl<'a> Transformer<'a> {
    pub(crate) fn transform_create_table(&mut self, _stmt: &'a CreateStmt) -> Result<SqlStatementKind> {
        Err(pending("CREATE TABLE"))
    }

    pub(crate) fn transform_create_schema(
        &mut self,
        _stmt: &'a CreateSchemaStmt,
    ) -> Result<SqlStatementKind> {
        Err(pending("CREATE SCHEMA"))
    }

    pub(crate) fn transform_create_view(&mut self, _stmt: &'a ViewStmt) -> Result<SqlStatementKind> {
        Err(pending("CREATE VIEW"))
    }

    pub(crate) fn transform_create_sequence(&mut self, _stmt: &'a CreateSeqStmt) -> Result<SqlStatementKind> {
        Err(pending("CREATE SEQUENCE"))
    }

    pub(crate) fn transform_create_function(
        &mut self,
        _stmt: &'a CreateFunctionStmt,
    ) -> Result<SqlStatementKind> {
        Err(pending("CREATE MACRO"))
    }

    pub(crate) fn transform_create_table_as(
        &mut self,
        _stmt: &'a CreateTableAsStmt,
    ) -> Result<SqlStatementKind> {
        Err(pending("CREATE TABLE AS"))
    }

    pub(crate) fn transform_create_index(&mut self, _stmt: &'a IndexStmt) -> Result<SqlStatementKind> {
        Err(pending("CREATE INDEX"))
    }

    pub(crate) fn transform_create_type(&mut self, _stmt: &'a CreateTypeStmt) -> Result<SqlStatementKind> {
        Err(pending("CREATE TYPE"))
    }

    pub(crate) fn transform_secret(&mut self, _stmt: &'a CreateSecretStmt) -> Result<SqlStatementKind> {
        Err(pending("CREATE SECRET"))
    }

    pub(crate) fn transform_drop(&mut self, _stmt: &'a DropStmt) -> Result<SqlStatementKind> {
        Err(pending("DROP"))
    }

    pub(crate) fn transform_drop_secret(&mut self, _stmt: &'a DropSecretStmt) -> Result<SqlStatementKind> {
        Err(pending("DROP SECRET"))
    }

    pub(crate) fn transform_alter(&mut self, _stmt: &'a AlterTableStmt) -> Result<SqlStatementKind> {
        Err(pending("ALTER TABLE"))
    }

    pub(crate) fn transform_alter_database(
        &mut self,
        _stmt: &'a AlterDatabaseStmt,
    ) -> Result<SqlStatementKind> {
        Err(pending("ALTER DATABASE"))
    }

    pub(crate) fn transform_alter_sequence(&mut self, _stmt: &'a AlterSeqStmt) -> Result<SqlStatementKind> {
        Err(pending("ALTER SEQUENCE"))
    }

    pub(crate) fn transform_rename(&mut self, _stmt: &'a RenameStmt) -> Result<SqlStatementKind> {
        Err(pending("ALTER ... RENAME"))
    }

    pub(crate) fn transform_comment_on(&mut self, _stmt: &'a CommentOnStmt) -> Result<SqlStatementKind> {
        Err(pending("COMMENT ON"))
    }
}
