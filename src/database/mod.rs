pub mod seq_access;
pub mod table_access;

use std::{cell::RefCell, fs::create_dir, num::ParseIntError, path::Path};

use thiserror::Error;

use crate::{
    data::page::{PageDataLayout, PageDataLayoutError},
    database::{
        seq_access::{SeqAccess, SeqAccessError},
        table_access::{TableAccess, TableAccessError},
    },
    freespace::fsm::Fsm,
    schema::{
        Column, ColumnType, TableSchema,
        table::{Cell, Row, Table},
    },
    store::{Store, StoreError, file_store::FileStore},
    tree::store::BTreeStore,
};

// TODO: define constants for system catalog
// Not a good solution for NULL, but very simple for now (see comment in btree module)
pub const NULL_INT: i32 = i32::MIN;
// Page size should not be smaller than 512 bytes, where the FSM implementation needs at least 256
pub const PAGE_SIZE: u16 = 4096;

pub struct Database<S: Store> {
    pub name: String,
    store: S,
    layout: PageDataLayout,
}

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("Invalid database name: {0}")]
    InvalidName(String),
    #[error("Table not found: {0}")]
    TableNotFound(String),
    #[error("Corrupted database: {0}")]
    CorruptedDatabase(String),
    #[error("Database store error")]
    Store(#[from] StoreError),
    #[error("Database table access error")]
    TableAccess(#[from] TableAccessError),
    #[error("Database I/O error")]
    Io(#[from] std::io::Error),
    #[error("Database page layout error")]
    PageLayout(#[from] PageDataLayoutError),
}

pub struct CreateColumnCommand {
    name: String,
    col_type: ColumnType,
    has_sequence: bool,
    is_unique: bool,
}

impl From<(&str, ColumnType)> for CreateColumnCommand {
    fn from(value: (&str, ColumnType)) -> Self {
        Self {
            name: value.0.to_owned(),
            col_type: value.1,
            has_sequence: false,
            is_unique: false,
        }
    }
}

impl From<(&str, ColumnType, bool)> for CreateColumnCommand {
    fn from(value: (&str, ColumnType, bool)) -> Self {
        Self {
            name: value.0.to_owned(),
            col_type: value.1,
            has_sequence: value.2,
            is_unique: false,
        }
    }
}

impl From<(&str, ColumnType, bool, bool)> for CreateColumnCommand {
    fn from(value: (&str, ColumnType, bool, bool)) -> Self {
        Self {
            name: value.0.to_owned(),
            col_type: value.1,
            has_sequence: value.2,
            is_unique: value.3,
        }
    }
}

#[derive(Debug, Error)]
pub enum CreateTableError {
    #[error("Invalid schema definition: {0}")]
    InvalidSchemaDefinition(String),
    #[error("Table with the same name already exists")]
    TableAlreadyExists,
    #[error("Table creation unknown error: {0}")]
    UnknownError(String),
}

impl From<DatabaseError> for CreateTableError {
    fn from(err: DatabaseError) -> Self {
        CreateTableError::UnknownError(err.to_string())
    }
}

impl From<SeqAccessError> for CreateTableError {
    fn from(err: SeqAccessError) -> Self {
        CreateTableError::UnknownError(err.to_string())
    }
}

impl From<TableAccessError> for CreateTableError {
    fn from(err: TableAccessError) -> Self {
        CreateTableError::UnknownError(err.to_string())
    }
}

impl From<StoreError> for CreateTableError {
    fn from(err: StoreError) -> Self {
        CreateTableError::UnknownError(err.to_string())
    }
}

fn is_safe_dir_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl Database<FileStore> {
    pub fn new(name: &str) -> Result<Self, DatabaseError> {
        let mut do_init = false;
        if !is_safe_dir_name(name) {
            return Err(DatabaseError::InvalidName(
                "Database names only allow alphanumeric chars and '_', '-'".to_owned(),
            ));
        }
        let path = Path::new(".").join(name);

        if !path.exists() {
            create_dir(path.clone())?;
            do_init = true;
        }

        let store = FileStore::new(&path)?;

        let db = Self {
            store,
            name: name.to_owned(),
            layout: PageDataLayout::new(PAGE_SIZE)?,
        };

        if do_init {
            db.init()?;
        }

        Ok(db)
    }
}

impl<S: Store> Database<S> {
    pub fn new_with_store(name: &str, store: S) -> Result<Self, DatabaseError> {
        Ok(Self {
            name: name.to_string(),
            store,
            layout: PageDataLayout::new(PAGE_SIZE)?,
        })
    }

    pub fn drop_create(&self) -> Result<(), DatabaseError> {
        self.store.delete_all()?;
        self.init()?;
        Ok(())
    }

    fn table_instance(&self) -> Table {
        let table_schema = TableSchema::new(vec![
            Column::new(10, "id", ColumnType::Int),
            Column::new(20, "name", ColumnType::Varchar(512)),
        ]);

        Table::new(1, "tables".to_owned(), table_schema)
    }

    fn col_table_instance(&self) -> Table {
        let col_schema = TableSchema::new(vec![
            Column::new(30, "id", ColumnType::Int),
            Column::new(40, "t_id", ColumnType::Int),
            Column::new(50, "name", ColumnType::Varchar(512)),
            Column::new(60, "type", ColumnType::Byte),
            Column::new(70, "length", ColumnType::Int),
        ]);

        Table::new(2, "columns".to_owned(), col_schema)
    }

    fn init_table_table(&self) -> Result<(), DatabaseError> {
        let table_table = self.table_instance();
        self.store.create(&self.layout, &table_table)?;
        let access = TableAccess::new(table_table.clone(), &self.store, &self.layout);
        let fsm_access = Fsm::access(&table_table);
        self.store.create(&self.layout, &fsm_access)?;

        let table_row_tables = Row::new(vec![Cell::Int(1), Cell::Varchar("tables".to_owned())]);
        let table_row_cols = Row::new(vec![Cell::Int(2), Cell::Varchar("columns".to_owned())]);
        let table_row_seq = Row::new(vec![Cell::Int(3), Cell::Varchar("sequences".to_owned())]);
        let table_row_indexes = Row::new(vec![Cell::Int(4), Cell::Varchar("indexes".to_owned())]);

        access.insert(&table_row_tables)?;
        access.insert(&table_row_cols)?;
        access.insert(&table_row_seq)?;
        access.insert(&table_row_indexes)?;

        Ok(())
    }

    fn init_column_table(&self) -> Result<(), DatabaseError> {
        // type can have these values
        // 0 - int
        // 1 - varchar
        // 2 - byte
        let col_table = self.col_table_instance();

        self.store.create(&self.layout, &col_table)?;
        let access = TableAccess::new(col_table.clone(), &self.store, &self.layout);
        let fsm_access = Fsm::access(&col_table);
        self.store.create(&self.layout, &fsm_access)?;

        // insert rows for columns table - "tables" table columns
        let col_row_tables_id = Row::new(vec![
            Cell::Int(10),
            Cell::Int(1),
            Cell::Varchar("id".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);
        let col_row_tables_name = Row::new(vec![
            Cell::Int(20),
            Cell::Int(1),
            Cell::Varchar("name".to_owned()),
            Cell::Byte(1), // ColumnType::Varchar
            Cell::Int(512),
        ]);

        // insert rows for columns table - "columns" table columns
        let col_row_cols_id = Row::new(vec![
            Cell::Int(30),
            Cell::Int(2),
            Cell::Varchar("id".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);
        let col_row_cols_t_id = Row::new(vec![
            Cell::Int(40),
            Cell::Int(2),
            Cell::Varchar("t_id".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);
        let col_row_cols_name = Row::new(vec![
            Cell::Int(50),
            Cell::Int(2),
            Cell::Varchar("name".to_owned()),
            Cell::Byte(1), // ColumnType::Varchar
            Cell::Int(512),
        ]);
        let col_row_cols_type = Row::new(vec![
            Cell::Int(60),
            Cell::Int(2),
            Cell::Varchar("type".to_owned()),
            Cell::Byte(2), // ColumnType::Byte
            Cell::Int(0),
        ]);
        let col_row_cols_length = Row::new(vec![
            Cell::Int(70),
            Cell::Int(2),
            Cell::Varchar("length".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);

        // insert rows for columns table - "sequences" table columns
        let col_row_seq_id = Row::new(vec![
            Cell::Int(80),
            Cell::Int(3),
            Cell::Varchar("id".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);

        // Even if there is only one sequence per column and it would make more sense to
        // link the sequence to the column with an seq_id field in columns,
        // this design avoids null handling and, therefore, the need to implement a null mechanism.
        let col_row_seq_col_id = Row::new(vec![
            Cell::Int(85),
            Cell::Int(3),
            Cell::Varchar("col_id".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);
        let col_row_seq_value = Row::new(vec![
            Cell::Int(90),
            Cell::Int(3),
            Cell::Varchar("current".to_owned()),
            Cell::Byte(0),
            Cell::Int(0),
        ]);

        // insert rows for columns table - "indexes" table columns

        // This design already includes the case, that an index can span over multiple columns.
        // But currently, only one column is supported.
        let col_row_idx_id = Row::new(vec![
            Cell::Int(100),
            Cell::Int(4),
            Cell::Varchar("id".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);

        let col_row_idx_t_id = Row::new(vec![
            Cell::Int(110),
            Cell::Int(4),
            Cell::Varchar("t_id".to_owned()),
            Cell::Byte(0), // ColumnType::Int
            Cell::Int(0),
        ]);

        // Just a Varchar because of the lack of an array type.
        // format is space separated list of ids (for composite indexes).
        // eg., "1" or "1 2" or "4 2 1"
        // Composite keys are currently not supported
        let col_row_idx_col_ids = Row::new(vec![
            Cell::Int(120),
            Cell::Int(4),
            Cell::Varchar("col_ids".to_owned()),
            Cell::Byte(1), // ColumnType::Varchar
            Cell::Int(512),
        ]);

        // Insert all column definitions
        access.insert(&col_row_tables_id)?;
        access.insert(&col_row_tables_name)?;

        access.insert(&col_row_cols_id)?;
        access.insert(&col_row_cols_t_id)?;
        access.insert(&col_row_cols_name)?;
        access.insert(&col_row_cols_type)?;
        access.insert(&col_row_cols_length)?;

        access.insert(&col_row_seq_id)?;
        access.insert(&col_row_seq_col_id)?;
        access.insert(&col_row_seq_value)?;

        access.insert(&col_row_idx_id)?;
        access.insert(&col_row_idx_t_id)?;
        access.insert(&col_row_idx_col_ids)?;

        Ok(())
    }

    fn init_sequences_table(&self) -> Result<(), DatabaseError> {
        let seq_schema = TableSchema::new(vec![
            Column::new(80, "id", ColumnType::Int),
            Column::new(85, "col_id", ColumnType::Int),
            Column::new(90, "current", ColumnType::Int),
        ]);

        let seq_table = Table::new(3, "sequences".to_owned(), seq_schema);
        self.store.create(&self.layout, &seq_table)?;

        let tbl_seq = TableAccess::new(seq_table.clone(), &self.store, &self.layout);
        let fsm_access = Fsm::access(&seq_table);
        self.store.create(&self.layout, &fsm_access)?;

        let seq_row_table_id = Row::new(vec![Cell::Int(1), Cell::Int(10), Cell::Int(4)]);
        let seq_row_column_id = Row::new(vec![Cell::Int(2), Cell::Int(30), Cell::Int(120)]);

        let seq_row_seq_id = Row::new(vec![Cell::Int(4), Cell::Int(80), Cell::Int(4)]);

        let seq_row_idx_id = Row::new(vec![Cell::Int(3), Cell::Int(100), Cell::Int(0)]);

        tbl_seq.insert(&seq_row_table_id)?;
        tbl_seq.insert(&seq_row_column_id)?;
        tbl_seq.insert(&seq_row_seq_id)?;
        tbl_seq.insert(&seq_row_idx_id)?;

        Ok(())
    }

    fn init_indexes_table(&self) -> Result<(), DatabaseError> {
        let idx_schema = TableSchema::new(vec![
            Column::new(100, "id", ColumnType::Int),
            Column::new(110, "t_id", ColumnType::Int),
            Column::new(120, "col_ids", ColumnType::Varchar(512)),
        ]);

        let idx_table = Table::new(4, "indexes".to_owned(), idx_schema);
        self.store.create(&self.layout, &idx_table)?;

        let fsm_access = Fsm::access(&idx_table);
        self.store.create(&self.layout, &fsm_access)?;

        Ok(())
    }

    fn init(&self) -> Result<(), DatabaseError> {
        // ignore unique index for base table ids for now
        self.init_table_table()?;
        self.init_column_table()?;
        self.init_sequences_table()?;
        self.init_indexes_table()?;

        Ok(())
    }

    pub fn seq_access_for_table<'db>(
        &'db self,
        table: Table,
    ) -> Result<SeqAccess<'db, S>, DatabaseError> {
        let seq_table = self.read_table("sequences")?;
        Ok(SeqAccess::new(
            TableAccess::new(seq_table, &self.store, &self.layout),
            table,
        ))
    }

    // Would be much nicer, if Table could be moved into TableAccess wrapped in an Rc or Arc
    // But this refactoring would take some time, so I chose this solution:
    // 1. read_table
    // 2. create TableAccess with reference to the table
    //
    // Same is valid for read_sequence_table and SeqAccess
    pub fn table_access<'db>(
        &'db self,
        table: Table,
    ) -> Result<TableAccess<'db, S>, DatabaseError> {
        // ignore the index table itself
        // means: the index table cannot have indexes at the moment (they are simply never read).
        // the problem here is the infinite recursion, it's fixable by using a cache of the catalog table indexes
        // as soon as the index of 'indexes' is cached, it's not necessary to read it again.
        if table.id() != 4 {
            let indexes = self.read_table("indexes")?;
            let idx_acc = self.table_access(indexes)?;
            let indexes = idx_acc.find("t_id", Cell::Int(table.id()))?;
            let id_idx = indexes.schema().find_index_by_name("id").ok_or_else(|| {
                DatabaseError::CorruptedDatabase(
                    "Table 'indexes' does not have an 'id' column".to_owned(),
                )
            })?;

            let col_ids_idx = indexes
                .schema()
                .find_index_by_name("col_ids")
                .ok_or_else(|| {
                    DatabaseError::CorruptedDatabase(
                        "Table 'indexes' does not have an 'col_ids' column".to_owned(),
                    )
                })?;

            // indexed_columns:
            // Vec of items column_id and BTree stores (Vec<i32, BTreeStore>)
            let indexed_columns = indexes.rows()?.into_iter().map(|(_, row)| {
                let btree_id = match &row.cells()[id_idx] {
                    Cell::Int(val) => val,
                    _ => {
                        return Err(DatabaseError::CorruptedDatabase("Column 'id' of table 'indexes' must be of type INT".to_owned()));
                    }
                };

                let cold_id = match &row.cells()[col_ids_idx] {
                    Cell::Varchar(val) => {
                        let col_ids = val.split(' ')
                            .map(|s| s.parse::<i32>())
                            .collect::<Result<Vec<i32>, ParseIntError>>()
                            .map_err(|_|
                                DatabaseError::CorruptedDatabase(
                                    "Column 'col_ids' of table 'indexes' has an invalid format. Valid would be '1', '5 1', etc...".to_owned()
                                )
                            )?;
                        if col_ids.len() > 1 {
                            return Err(DatabaseError::CorruptedDatabase(
                                "Composite index currently not supported. Found composite index".to_owned())
                            );
                        }
                        col_ids[0]
                    },
                    _ => {
                        return Err(DatabaseError::CorruptedDatabase("Column 'col_ids' of table 'indexes' must be of type VARCHAR".to_owned()));
                    }
                };

                let btree = RefCell::new(self.store.read_btree(*btree_id)?);
                Ok((cold_id, btree))
            }).collect::<Result<Vec<(i32, RefCell<BTreeStore>)>, DatabaseError>>()?;

            Ok(TableAccess::new(table, &self.store, &self.layout).with_indexes(indexed_columns))
        } else {
            Ok(TableAccess::new(table, &self.store, &self.layout))
        }
    }

    pub fn read_table(&self, table_name: &str) -> Result<Table, DatabaseError> {
        let table_table = self.table_instance();
        let access = TableAccess::new(table_table, &self.store, &self.layout);
        let table_query = access.find("name", Cell::Varchar(table_name.to_owned()))?;
        let table_id_index = table_query
            .schema()
            .find_index_by_name("id")
            .ok_or_else(|| {
                DatabaseError::CorruptedDatabase(
                    "Column 'id' not found in 'tables' table".to_owned(),
                )
            })?;
        let rows = table_query.rows()?;

        if rows.is_empty() {
            return Err(DatabaseError::TableNotFound(table_name.to_owned()));
        }
        if rows.len() > 1 {
            return Err(DatabaseError::CorruptedDatabase(format!(
                "Multiple occurences of: {}",
                table_name
            )));
        }

        let table_id = match rows[0].1.cells()[table_id_index] {
            Cell::Int(val) => val,
            _ => {
                return Err(DatabaseError::CorruptedDatabase(
                    "Column 'id' has wrong type in 'tables' table".to_owned(),
                ));
            }
        };

        // load schema
        let col_table = self.col_table_instance();
        let col_access = TableAccess::new(col_table, &self.store, &self.layout);
        let col_query = col_access.find("t_id", Cell::Int(table_id))?;
        let col_schema = col_query.schema();

        let id_index = col_schema.find_index_by_name("id").ok_or_else(|| {
            DatabaseError::CorruptedDatabase("Column 'id' not found in 'columns' table".to_owned())
        })?;
        let name_index = col_schema.find_index_by_name("name").ok_or_else(|| {
            DatabaseError::CorruptedDatabase(
                "Column 'name' not found in 'columns' table".to_owned(),
            )
        })?;
        let type_index = col_schema.find_index_by_name("type").ok_or_else(|| {
            DatabaseError::CorruptedDatabase(
                "Column 'type' not found in 'columns' table".to_owned(),
            )
        })?;
        let length_index = col_schema.find_index_by_name("length").ok_or_else(|| {
            DatabaseError::CorruptedDatabase(
                "Column 'length' not found in 'columns' table".to_owned(),
            )
        })?;

        let col_rows = col_query
            .rows()?
            .into_iter()
            .map(|(_, row)| {
                let id = match &row.cells()[id_index] {
                    Cell::Int(val) => val,
                    _ => {
                        return Err(DatabaseError::CorruptedDatabase(
                            "Column 'id' has wrong type in 'columns' table".to_owned(),
                        ));
                    }
                };

                let name = match &row.cells()[name_index] {
                    Cell::Varchar(val) => val,
                    _ => {
                        return Err(DatabaseError::CorruptedDatabase(
                            "Column 'name' has wrong type in 'columns' table".to_owned(),
                        ));
                    }
                };
                let length = match row.cells()[length_index] {
                    Cell::Int(val) => val,
                    _ => {
                        return Err(DatabaseError::CorruptedDatabase(
                            "Column 'length' has wrong type in 'columns' table".to_owned(),
                        ));
                    }
                };

                let col_type = match row.cells()[type_index] {
                    Cell::Byte(val) => {
                        match val {
                            0 => ColumnType::Int,
                            1 => ColumnType::Varchar(length as u16), // length is stored separately
                            2 => ColumnType::Byte,
                            _ => {
                                return Err(DatabaseError::CorruptedDatabase(format!(
                                    "Invalid column 'type' value: {}",
                                    val
                                )));
                            }
                        }
                    }
                    _ => {
                        return Err(DatabaseError::CorruptedDatabase(
                            "Column 'type' has wrong type in 'columns' table".to_owned(),
                        ));
                    }
                };

                Ok(Column::new(*id, name, col_type))
            })
            .collect::<Result<Vec<Column>, DatabaseError>>()?;

        let schema = TableSchema::new(col_rows);

        Ok(Table::new(table_id, table_name.to_owned(), schema))
    }

    pub fn drop_table(&self, name: &str) -> Result<(), DatabaseError> {
        // fetch table reference to call delete on store
        let table_to_drop = self.read_table(name)?;

        // delete table entry in tables
        let table_table = self.read_table("tables")?;
        let tbl_access = self.table_access(table_table)?;
        let tbl_query = tbl_access.find("name", Cell::Varchar(name.to_owned()))?;
        tbl_access.delete(tbl_query)?;

        // delete column entries in columns
        let col_table = self.read_table("columns")?;
        let col_access = self.table_access(col_table)?;

        let col_query = col_access.find("t_id", Cell::Int(table_to_drop.id()))?;
        col_access.delete(col_query)?;

        // delete sequence if exists
        // delete index if exists

        // drop pages file
        self.store.delete(&table_to_drop)?;
        Ok(())
    }

    pub fn create_table<C: Into<CreateColumnCommand>>(
        &self,
        name: &str,
        schema_command: Vec<C>,
    ) -> Result<Table, CreateTableError> {
        // check if unique index is only created on int
        // create columns
        let column_commands: Vec<CreateColumnCommand> =
            schema_command.into_iter().map(|c| c.into()).collect();
        // create table entry in tables
        let table_table = self.read_table("tables")?;
        let mut tbl_seq_acc = self.seq_access_for_table(table_table.clone())?;
        let access = self.table_access(table_table)?;

        // check if table with the same name already exists
        let existing_table_query = access.find("name", Cell::Varchar(name.to_owned()))?;
        if !existing_table_query.rows()?.is_empty() {
            return Err(CreateTableError::TableAlreadyExists);
        }

        let tbl_id = tbl_seq_acc.next_val("id")?;
        access.insert(&Row::new(vec![
            Cell::Int(tbl_id),
            Cell::Varchar(name.to_owned()),
        ]))?;

        // create column entries
        let col_table = self.read_table("columns")?;
        let mut col_seq_acc = self.seq_access_for_table(col_table.clone())?;
        let col_access = self.table_access(col_table)?;

        let mut columns = Vec::new();
        for cc in column_commands {
            if cc.is_unique && !matches!(cc.col_type, ColumnType::Int) {
                return Err(CreateTableError::InvalidSchemaDefinition(format!(
                    "Unique index can only be created on int columns. Column '{}' has type '{}'",
                    cc.name, cc.col_type
                )));
            }
            if cc.has_sequence && !matches!(cc.col_type, ColumnType::Int) {
                return Err(CreateTableError::InvalidSchemaDefinition(format!(
                    "Sequence can only be created on int columns. Column '{}' has type '{}'",
                    cc.name, cc.col_type
                )));
            }
            let col_id = col_seq_acc.next_val("id")?;
            let column = Column::new(col_id, &cc.name, cc.col_type);
            col_access.insert(&Row::new(vec![
                Cell::Int(col_id),
                Cell::Int(tbl_id),
                Cell::Varchar(cc.name),
                Cell::Byte(match column.col_type {
                    ColumnType::Int => 0,
                    ColumnType::Varchar(_) => 1,
                    ColumnType::Byte => 2,
                }),
                Cell::Int(match column.col_type {
                    ColumnType::Int => 0,
                    ColumnType::Varchar(len) => len as i32,
                    ColumnType::Byte => 0,
                }),
            ]))?;

            if cc.has_sequence {
                // Create sequence for column
                let seq_table = self.read_table("sequences")?;
                let mut seq_sequences = self.seq_access_for_table(seq_table.clone())?;
                let seq_acc = self.table_access(seq_table)?;

                seq_acc.insert(&Row::new(vec![
                    Cell::Int(seq_sequences.next_val("id")?),
                    Cell::Int(col_id),
                    Cell::Int(0),
                ]))?;
            }

            if cc.is_unique {
                // Create index for table and column
                let idx_table = self.read_table("indexes")?;
                let mut idx_sequences = self.seq_access_for_table(idx_table.clone())?;
                let idx_acc = self.table_access(idx_table)?;

                idx_acc.insert(&Row::new(vec![
                    Cell::Int(idx_sequences.next_val("id")?),
                    Cell::Int(tbl_id),
                    Cell::Varchar(col_id.to_string()),
                ]))?;
            }
            columns.push(column);
        }

        let schema = TableSchema::new(columns);
        let new_table = Table::new(tbl_id, name.to_owned(), schema);

        self.store.create(&self.layout, &new_table)?;
        self.create_fsm(&new_table)?;

        Ok(new_table)
    }

    fn create_fsm(&self, table: &Table) -> Result<(), CreateTableError> {
        let fsm_access = Fsm::access(table);
        self.store.create(&self.layout, &fsm_access)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use crate::{
        database::{CreateTableError, Database, DatabaseError},
        schema::{
            ColumnType,
            table::{Cell, Row},
        },
        store::file_store::FileStore,
    };

    #[test]
    fn should_contain_base_tables_after_init_db() {
        // Arrange
        let base_path = tempfile::tempdir().unwrap();
        let store = FileStore::new(base_path.path()).unwrap();
        let db = Database::new_with_store("test_db", store).unwrap();

        // Act
        db.drop_create().unwrap();

        // Assert
        let table_tables = db.read_table("tables").unwrap();
        let access = db.table_access(table_tables).unwrap();
        let table_entries = access
            .find_all()
            .unwrap()
            .rows()
            .unwrap()
            .into_iter()
            .map(|(_, row)| row)
            .collect::<Vec<Row>>();

        // Check catalog tables:
        assert!(table_entries.contains(&Row::new(vec![
            Cell::Int(1),
            Cell::Varchar("tables".to_owned())
        ])));
        assert!(table_entries.contains(&Row::new(vec![
            Cell::Int(2),
            Cell::Varchar("columns".to_owned())
        ])));
        assert!(table_entries.contains(&Row::new(vec![
            Cell::Int(3),
            Cell::Varchar("sequences".to_owned())
        ])));
        assert!(table_entries.contains(&Row::new(vec![
            Cell::Int(4),
            Cell::Varchar("indexes".to_owned())
        ])));

        let table_tables = db.read_table("columns").unwrap();
        let access = db.table_access(table_tables).unwrap();
        let column_entries = access
            .find_all()
            .unwrap()
            .rows()
            .unwrap()
            .into_iter()
            .map(|(_, row)| row)
            .collect::<Vec<Row>>();

        // Just a sample of columns:
        // table columns:
        assert!(column_entries.contains(&Row::new(vec![
            Cell::Int(10),                  // id
            Cell::Int(1),                   // t_id
            Cell::Varchar("id".to_owned()), // name
            Cell::Byte(0),                  // type
            Cell::Int(0),                   // length
        ])));

        assert!(column_entries.contains(&Row::new(vec![
            Cell::Int(20),                    // id
            Cell::Int(1),                     // t_id
            Cell::Varchar("name".to_owned()), // name
            Cell::Byte(1),                    // type
            Cell::Int(512),                   // length
        ])));
    }

    #[test]
    fn should_be_able_to_find_table_after_created() {
        // Arrange Database
        let base_path = tempfile::tempdir().unwrap();
        let store = FileStore::new(base_path.path()).unwrap();
        let db = Database::new_with_store("test_db", store).unwrap();

        // Act
        db.drop_create().unwrap();
        db.create_table(
            "persons",
            vec![
                ("id", ColumnType::Int),
                ("name", ColumnType::Varchar(255)),
                ("age", ColumnType::Int),
            ],
        )
        .unwrap();

        // Assert
        let table = db.read_table("persons").unwrap();
        let access = db.table_access(table).unwrap();
        access
            .insert(&Row::new(vec![
                Cell::Int(1),
                Cell::Varchar("Alice".to_owned()),
                Cell::Int(30),
            ]))
            .unwrap();

        let query_result = access.find("id", Cell::Int(1)).unwrap();
        let rows = query_result.rows().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].1.cells(),
            &[
                Cell::Int(1),
                Cell::Varchar("Alice".to_owned()),
                Cell::Int(30)
            ]
        );

        // second creation should fail
        let result = db.create_table(
            "persons",
            vec![
                ("id", ColumnType::Int),
                ("name", ColumnType::Varchar(255)),
                ("age", ColumnType::Int),
            ],
        );

        assert!(matches!(result, Err(CreateTableError::TableAlreadyExists)));
    }

    #[test]
    fn should_be_deleted_completely_after_dropped() {
        // Arrange Database
        let base_path = tempfile::tempdir().unwrap();
        let store = FileStore::new(base_path.path()).unwrap();
        let db = Database::new_with_store("test_db", store).unwrap();

        // Act
        db.drop_create().unwrap();
        db.create_table(
            "persons",
            vec![
                ("id", ColumnType::Int),
                ("name", ColumnType::Varchar(255)),
                ("age", ColumnType::Int),
            ],
        )
        .unwrap();

        // Assert
        let table = db.read_table("persons").unwrap();
        let table_id = table.id();
        let access = db.table_access(table).unwrap();
        access
            .insert(&Row::new(vec![
                Cell::Int(1),
                Cell::Varchar("Alice".to_owned()),
                Cell::Int(30),
            ]))
            .unwrap();

        db.drop_table("persons").unwrap();
        let result = db.read_table("persons");
        assert!(matches!(result, Err(DatabaseError::TableNotFound(_))));

        let tbl_table = db.read_table("tables").unwrap();
        let access = db.table_access(tbl_table).unwrap();
        let query_result = access
            .find("name", Cell::Varchar("persons".to_owned()))
            .unwrap();
        assert_eq!(query_result.rows().unwrap().len(), 0);

        let col_table = db.read_table("columns").unwrap();
        let col_access = db.table_access(col_table).unwrap();
        let query_result = col_access.find("t_id", Cell::Int(table_id)).unwrap();
        assert_eq!(query_result.rows().unwrap().len(), 0);
    }
}
