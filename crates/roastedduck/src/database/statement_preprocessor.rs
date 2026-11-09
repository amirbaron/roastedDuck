//! `StatementPreprocessor`: expose actual body statements and transaction handling.
use std::io::Read;

use roastedduck_ast::{
    ParsedExpression, SetScope, SetStatement, SqlStatement, SqlStatementKind, TransactionInfo,
    TransactionInvalidationPolicy, TransactionModifierType, TransactionType,
};
use roastedduck_common::{ByteString, Error, ErrorKind, Result};
use roastedduck_types::Value;

use crate::Connection;

fn transaction(transaction_type: TransactionType) -> SqlStatement {
    let query = if transaction_type == TransactionType::BeginTransaction {
        "BEGIN;"
    } else {
        "COMMIT;"
    };
    let mut statement = SqlStatement::new(SqlStatementKind::Transaction(TransactionInfo {
        transaction_type,
        modifier: TransactionModifierType::TransactionDefaultModifier,
        invalidation_policy: TransactionInvalidationPolicy::AllErrorsInvalidateTransaction,
        auto_rollback: true,
    }));
    statement.query = query.into();
    statement
}

fn policy(policy: TransactionInvalidationPolicy) -> SqlStatement {
    let mut statement = SqlStatement::new(SqlStatementKind::Set(SetStatement {
        name: "current_transaction_invalidation_policy".into(),
        scope: SetScope::Global,
        value: Some(Box::new(ParsedExpression::constant(Value::varchar(
            policy.name(),
        )))),
    }));
    statement.query = format!(
        "SET GLOBAL current_transaction_invalidation_policy TO '{}';",
        policy.name()
    )
    .into();
    statement
}

fn append_body(
    result: &mut Vec<SqlStatement>,
    mut body: Vec<SqlStatement>,
    active_transaction: bool,
    skip_handling: bool,
) {
    let transactional = body.len() > 1 && !skip_handling;
    if transactional {
        result.push(if active_transaction {
            policy(TransactionInvalidationPolicy::AllErrorsInvalidateTransaction)
        } else {
            transaction(TransactionType::BeginTransaction)
        });
    }
    result.append(&mut body);
    if transactional {
        result.push(if active_transaction {
            policy(TransactionInvalidationPolicy::StandardPolicy)
        } else {
            transaction(TransactionType::Commit)
        });
    }
}

fn import_io_error(path: &[u8], error: &std::io::Error) -> Error {
    roastedduck_storage::file_system::callback_error(error).unwrap_or_else(|| {
        Error::new_bytes(
            ErrorKind::Io,
            roastedduck_common::byte_format!(
                "Cannot open file \"{path}\": {}",
                roastedduck_common::os_error_text(error)
            ),
        )
    })
}

impl Connection {
    pub(super) fn preprocess_parsed_statements(
        &mut self,
        statements: Vec<SqlStatement>,
    ) -> Result<Vec<SqlStatement>> {
        if !statements.iter().any(|statement| {
            matches!(
                statement.kind,
                SqlStatementKind::Pragma(_) | SqlStatementKind::Multi(_)
            )
        }) {
            return Ok(statements);
        }
        if self.transactions.has_current() && self.transactions.is_invalidated() {
            return Err(crate::transaction::TransactionContext::aborted_error());
        }
        let initial_transaction = !self.transactions.is_auto_commit();
        let require_new = self.transactions.is_auto_commit() && !self.transactions.has_current();
        self.transactions.current();
        let outcome = self.preprocess_in_transaction(statements, initial_transaction);
        let invalidates = outcome
            .as_ref()
            .is_err_and(|error| super::invalidates_transaction(error.kind()));
        let ended = if require_new || !self.transactions.is_auto_commit() {
            self.end_client_statement(outcome.is_ok(), invalidates)
        } else {
            Ok(())
        };
        outcome.and_then(|statements| ended.map(|()| statements))
    }

    fn preprocess_in_transaction(
        &mut self,
        statements: Vec<SqlStatement>,
        initial_transaction: bool,
    ) -> Result<Vec<SqlStatement>> {
        let mut chained_transaction = false;
        let mut result = Vec::with_capacity(statements.len());
        for statement in statements {
            let active = initial_transaction || chained_transaction;
            match statement.kind {
                SqlStatementKind::Transaction(ref info) => {
                    match info.transaction_type {
                        TransactionType::BeginTransaction => chained_transaction = true,
                        TransactionType::Commit | TransactionType::Rollback => chained_transaction = false,
                        TransactionType::Invalid => {}
                    }
                    result.push(statement);
                }
                SqlStatementKind::Multi(body) => {
                    let has_select = body
                        .iter()
                        .any(|child| matches!(child.kind, SqlStatementKind::Select(_)));
                    append_body(&mut result, body, active, has_select);
                }
                SqlStatementKind::Pragma(ref pragma) => {
                    let name = pragma.name.clone();
                    let parameters = self.bind_pragma_parameters(statement.clone())?;
                    let body = if name.eq_ignore_ascii_case("import_database") {
                        self.import_database_statements(&parameters)?
                    } else {
                        let instance = self.database.instance();
                        let query = if instance.has_extension_pragma(&name) {
                            Some(instance.extension_pragma_query(&name, &parameters)?)
                        } else {
                            crate::system_functions::pragma_query(&name, &parameters)?
                        };
                        let Some(query) = query else {
                            result.push(statement);
                            continue;
                        };
                        roastedduck_parser::parse(&query)?
                    };
                    append_body(&mut result, body, active, false);
                }
                _ => result.push(statement),
            }
        }
        Ok(result)
    }

    /// Expand the actual export files through the current filesystem, retaining byte
    /// paths and rebasing each COPY to the supplied import directory.
    pub(super) fn import_database_statements(&self, parameters: &[Value]) -> Result<Vec<SqlStatement>> {
        let _scope = self.query_scope();
        let directory = parameters
            .first()
            .map(Value::to_varchar_bytes)
            .transpose()?
            .unwrap_or_default();
        let join = |name: &[u8]| -> ByteString {
            if directory.ends_with(b"/") {
                roastedduck_common::byte_format!("{directory}{name}")
            } else {
                roastedduck_common::byte_format!("{directory}/{name}")
            }
        };
        let mut statements = Vec::new();
        for name in ["schema.sql", "load.sql"] {
            let path = join(name.as_bytes());
            roastedduck_function::file_system::verify_can_access_file(&path)?;
            let filesystem = roastedduck_storage::file_system::DatabaseFileSystem::for_path(&path);
            let mut file = filesystem
                .open(
                    &path,
                    roastedduck_function::registered_files::OpenFlags {
                        read: true,
                        write: false,
                        create: false,
                        truncate: false,
                    },
                )
                .map_err(|error| import_io_error(&path, &error))?;
            let size = file.size().map_err(|error| import_io_error(&path, &error))?;
            let size =
                usize::try_from(size).map_err(|_| Error::invalid_input("Import SQL file is too large"))?;
            let mut query = vec![0; size];
            file.read_exact(&mut query)
                .map_err(|error| import_io_error(&path, &error))?;
            for mut statement in
                roastedduck_parser::parse_bytes_with_options(&query, &self.settings.parser_options())?
            {
                if name == "load.sql"
                    && let SqlStatementKind::Copy(copy) = &mut statement.kind
                {
                    let basename = copy
                        .file_path
                        .rsplit(|byte| matches!(byte, b'/' | b'\\'))
                        .next()
                        .unwrap_or_default();
                    copy.file_path = join(basename);
                }
                statements.push(statement);
            }
        }
        Ok(statements)
    }
}
