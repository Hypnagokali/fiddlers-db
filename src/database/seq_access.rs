use thiserror::Error;

/// Thin wrapper around TableAccess to provide sequence operations.
use crate::{
    database::table_access::{TableAccess, TableAccessError},
    schema::{
        ColumnType,
        table::{Cell, Table},
    },
    store::Store,
};

pub struct SeqAccess<'db, S: Store> {
    sequence_acc: TableAccess<'db, S>,
    table: Table,
}

#[derive(Error, Debug)]
pub enum SeqAccessError {
    #[error("Column '{0}' is not a sequence column (only Int supported)")]
    NotASequenceColumn(String),
    #[error("Column not found: {0}")]
    ColumnNotFound(String),
    #[error("Not a sequence")]
    NotASequence,
    #[error("Sequence not found")]
    SequenceNotFound,
    #[error("Sequence is corrupted: {0}")]
    SequenceCorrupted(String),
    #[error("Unknown error: {0}")]
    UnknownError(String),
}

impl From<TableAccessError> for SeqAccessError {
    fn from(err: TableAccessError) -> Self {
        SeqAccessError::UnknownError(err.to_string())
    }
}

impl<'db, S: Store> SeqAccess<'db, S> {
    pub fn new(sequence_acc: TableAccess<'db, S>, table: Table) -> Self {
        Self {
            sequence_acc,
            table,
        }
    }

    pub fn next_val(&mut self, column: &str) -> Result<i32, SeqAccessError> {
        let col_id = self
            .table
            .schema()
            .find_index_by_name(column)
            .ok_or(SeqAccessError::ColumnNotFound(column.to_owned()))
            .and_then(|col_idx| {
                let col_id_cell = &self.table.schema().columns[col_idx];
                match col_id_cell.col_type {
                    ColumnType::Int => Ok(col_id_cell.id),
                    _ => Err(SeqAccessError::NotASequenceColumn(column.to_owned())),
                }
            })?;

        let seq_query = self.sequence_acc.find("col_id", Cell::Int(col_id))?;
        let current_idx = seq_query
            .schema()
            .find_index_by_name("current")
            .ok_or(SeqAccessError::NotASequence)?;

        let seq = seq_query.rows()?;

        if seq.is_empty() {
            return Err(SeqAccessError::SequenceNotFound);
        }
        if seq.len() > 1 {
            return Err(SeqAccessError::SequenceCorrupted(format!(
                "Multiple sequences found for col_id {}",
                col_id
            )));
        }

        let next_val = match &seq[0].1.cells()[current_idx] {
            Cell::Int(val) => val + 1,
            _ => {
                return Err(SeqAccessError::SequenceCorrupted(format!(
                    "'current' is not an integer for col_id {}",
                    col_id
                )));
            }
        };

        self.sequence_acc.update(
            self.sequence_acc.find("col_id", Cell::Int(col_id))?,
            vec![("current", Cell::Int(next_val))],
        )?;

        Ok(next_val)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        data::page::PageDataLayout,
        database::{seq_access::SeqAccess, table_access::TableAccess},
        freespace::fsm::Fsm,
        schema::{
            Column, ColumnType, TableSchema,
            table::{Cell, Row, Table},
        },
        store::{Store, file_store::FileStore},
    };
    use tempfile::tempdir;

    #[test]
    fn find_should_return_error_if_cell_has_wrong_type() {
        let seq_schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int),
            Column::new(2, "col_id", ColumnType::Int),
            Column::new(3, "current", ColumnType::Int),
        ]);

        let seq_table = Table::new(1, "sequences".to_owned(), seq_schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path()).unwrap();
        let layout = PageDataLayout::new(1024).unwrap();

        store.create(&layout, &seq_table).unwrap();
        let access = TableAccess::new(seq_table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&seq_table);
        store.create(&layout, &fsm_access).unwrap();

        let my_seq = Row::new(vec![Cell::Int(1), Cell::Int(322), Cell::Int(0)]);

        access.insert(&my_seq).unwrap();

        // dummy table
        let dummy_schema = TableSchema::new(vec![
            Column::new(322, "id", ColumnType::Int),
            Column::new(323, "name", ColumnType::Varchar(64)),
        ]);

        let dummy_table = Table::new(2, "dummy".to_owned(), dummy_schema);

        let mut seq_access = SeqAccess::new(access, dummy_table);
        assert_eq!(seq_access.next_val("id").unwrap(), 1);
        assert_eq!(seq_access.next_val("id").unwrap(), 2);
        assert_eq!(seq_access.next_val("id").unwrap(), 3);
    }
}
