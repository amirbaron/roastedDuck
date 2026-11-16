//! Pinned PostgreSQL mutation algorithms: CTID batches and temporary update data.
use super::transaction::PgTransaction;
use roastedduck_catalog::backend::TableMutation;
use roastedduck_common::{Error, Result};
use roastedduck_types::{DataChunk, LogicalType, Value};
use std::sync::Arc;

pub(super) fn insert(pg: Arc<PgTransaction>, sql: String, text: bool) -> Box<dyn TableMutation> {
    Box::new(Insert {
        pg,
        sql,
        text,
        count: 0,
    })
}
#[derive(Debug)]
struct Insert {
    pg: Arc<PgTransaction>,
    sql: String,
    text: bool,
    count: u64,
}
impl TableMutation for Insert {
    fn sink(&mut self, chunk: &DataChunk) -> Result<()> {
        self.count += self
            .pg
            .copy_input(&self.sql, self.text, &mut std::iter::once(Ok(chunk.clone())))?;
        Ok(())
    }
    fn finish(&mut self) -> Result<u64> {
        Ok(self.count)
    }
}

fn row_id(value: &Value) -> Result<String> {
    let id = value
        .as_i64()
        .ok_or_else(|| Error::invalid_input("Postgres mutation requires a non-NULL CTID"))?;
    Ok(format!("'({},{})'", id >> 16, id & 0xffff))
}
pub(super) fn delete(
    pg: Arc<PgTransaction>,
    relation: String,
    row_id_index: usize,
) -> Box<dyn TableMutation> {
    Box::new(Delete {
        pg,
        relation,
        row_id_index,
        ctids: String::new(),
        count: 0,
    })
}
#[derive(Debug)]
struct Delete {
    pg: Arc<PgTransaction>,
    relation: String,
    row_id_index: usize,
    ctids: String,
    count: u64,
}
impl Delete {
    fn flush(&mut self) -> Result<()> {
        if !self.ctids.is_empty() {
            self.pg.execute(
                &format!("DELETE FROM {} WHERE ctid IN ({})", self.relation, self.ctids),
                true,
            )?;
            self.ctids.clear();
        }
        Ok(())
    }
}
impl TableMutation for Delete {
    fn sink(&mut self, chunk: &DataChunk) -> Result<()> {
        for row in 0..chunk.len() {
            if !self.ctids.is_empty() {
                self.ctids.push(',');
            }
            self.ctids
                .push_str(&row_id(&chunk.column(self.row_id_index).get_value(row))?);
            if self.ctids.len() > 3000 {
                self.flush()?;
            }
        }
        self.count += chunk.len() as u64;
        Ok(())
    }
    fn finish(&mut self) -> Result<u64> {
        self.flush()?;
        Ok(self.count)
    }
}

pub(super) fn update(
    pg: Arc<PgTransaction>,
    relation: &str,
    names: &[String],
    types: &[LogicalType],
    defaults: bool,
) -> Result<Box<dyn TableMutation>> {
    let mut random = roastedduck_function::random::RandomEngine::default();
    let bits =
        (u128::from(random.next_random_integer64()) << 64) | u128::from(random.next_random_integer64());
    let name = format!("update_data_{}", roastedduck_types::Uuid::from_u128(bits));
    let quoted = super::identifier(&name);
    let definitions = names
        .iter()
        .zip(types)
        .map(|(n, t)| {
            Ok(format!(
                "{} {}",
                super::identifier(n),
                roastedduck_postgres::copy::type_sql(t)?
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    pg.execute(
        &format!(
            "CREATE LOCAL TEMPORARY TABLE {quoted} ({}, __page_id_string VARCHAR) ON COMMIT DROP",
            definitions.join(",")
        ),
        true,
    )?;
    let assignments = names
        .iter()
        .map(|n| format!("{}={quoted}.{}", super::identifier(n), super::identifier(n)))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "UPDATE {relation} AS __pg_target SET {assignments} FROM {quoted} WHERE __pg_target.ctid=__page_id_string::TID"
    );
    let copy = format!("COPY {quoted} FROM STDIN (FORMAT TEXT, NULL '\u{0008}')");
    Ok(Box::new(Update {
        pg,
        sql,
        copy,
        types: types.to_vec(),
        defaults,
        count: 0,
    }))
}
#[derive(Debug)]
struct Update {
    pg: Arc<PgTransaction>,
    sql: String,
    copy: String,
    types: Vec<LogicalType>,
    defaults: bool,
    count: u64,
}
impl TableMutation for Update {
    fn sink(&mut self, chunk: &DataChunk) -> Result<()> {
        if self.defaults && !chunk.is_empty() {
            return Err(Error::binder(
                "SET DEFAULT is not yet supported for updates of a Postgres table",
            ));
        }
        let mut types = self.types.clone();
        types.push(LogicalType::VARCHAR);
        let mut output = DataChunk::with_types(&types, chunk.len());
        for row in 0..chunk.len() {
            for column in 0..self.types.len() {
                output
                    .column_mut(column)
                    .set_value(row, &chunk.column(column).get_value(row))?;
            }
            output.column_mut(self.types.len()).set_value(
                row,
                &Value::varchar(row_id(&chunk.column(self.types.len()).get_value(row))?),
            )?;
        }
        output.set_len(chunk.len());
        self.count += self
            .pg
            .copy_input(&self.copy, true, &mut std::iter::once(Ok(output)))?;
        Ok(())
    }
    fn finish(&mut self) -> Result<u64> {
        self.pg.execute(&self.sql, true)?;
        Ok(self.count)
    }
}
