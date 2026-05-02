use std::{cell::RefCell, collections::HashMap};

use thiserror::Error;

use crate::{data::page::{Page, PageDataLayout, PageError, Record}, fsm::fsm::Fsm, store::{IndexedRowIterator, PageRowIterator, Store}, table::{Column, TableSchema, table::{Cell, Row, RowValidationError, Table}}, tree::store::BTreeStore};

pub struct TableAccess<'db, S> {
    table: Table,
    // Having BTreeStore as a normal ref would fore TableAccess to be mutable everywhere
    indexed_columns: Vec<(i32, RefCell<BTreeStore>)>,
    store: &'db S,
    layout: &'db PageDataLayout,
    #[cfg(test)]
    index_used: RefCell<Vec<i32>>, // just values from find clause
}

#[derive(Error, Debug)]
pub enum TableAccessError {
    #[error("TableAccessError - insert error: {0}")]
    InsertRowError(String),
    #[error("TableAccessError - load error: {0}")]
    LoadRowsError(String),
    #[error("TableAccessError - update error: {0}")]
    UpdateRowsError(String),
    #[error("TableAccessError - delete error: {0}")]
    DeleteRowsError(String),
}

struct UpdateIndexCommand {
    // pointer to BTree in (indexed_columns), old or to-delete value, new value
    update_cells: Vec<(usize, Option<i32>, Option<i32>)>,
}

impl UpdateIndexCommand {
    fn new() -> Self {
        Self {
            update_cells: Vec::new(),
        }
    }

    fn push_update(&mut self, cell_update: (usize, i32, i32)) {
        self.update_cells.push((
            cell_update.0,
            Some(cell_update.1),
            Some(cell_update.2),
        ));
    }

    fn push_insert(&mut self, cell_update: (usize, i32)) {
        self.update_cells.push((
            cell_update.0,
            None,
            Some(cell_update.1),
        ));
    }

    fn push_delete(&mut self, cell_update: (usize, i32)) {
        self.update_cells.push((
            cell_update.0,
            Some(cell_update.1),
            None
        ));
    }

}


pub struct QueryResult<'db, I> {
    row_iter: Box<dyn Iterator<Item = I> +'db>,
    schema: TableSchema,
}

impl<'db, I: 'db> QueryResult<'db, I> {
    pub fn rows(self) -> Vec<I> {
        self.row_iter.into_iter().collect()
    }

    pub fn schema(&self) -> &TableSchema {
        &self.schema
    }

    pub fn filter<F: FnMut(&I) -> bool + 'db>(self, f: F) -> QueryResult<'db, I> {
        let iter = self.row_iter.filter(f);
        QueryResult { 
            row_iter: Box::new(iter),
            schema: self.schema,
        }
    }
}

impl<'db> QueryResult<'db, (Record, Row)> {
    pub fn from_indexes<S: Store>(
        index_iter: IndexedRowIterator<'_, S>,
        schema: TableSchema,
    ) -> QueryResult<'_, (Record, Row)> {
        QueryResult {
            row_iter: Box::new(index_iter),
            schema: schema.clone()
        }
    }

    pub fn new<I: Iterator<Item = Page<'db>> +'db>(
        page_iter: I,
        schema: TableSchema,
    ) -> QueryResult<'db, (Record, Row)> {

        let schema_iter = schema.clone();
        let i = page_iter.flat_map(move |p| {
            PageRowIterator::new(p, schema_iter.clone())
        });

        QueryResult {
            row_iter: Box::new(i),
            schema: schema.clone()
        }
    }

    pub fn hash_join(
        self,
        inner_query: QueryResult<'db, (Record, Row)>,
        this_join_column: &str, 
        that_join_column: &str
    ) -> Result<QueryResult<'db, Row>, TableAccessError> {
        let that_col_index = find_column_for_query(&inner_query.schema, that_join_column)?;
        let this_col_index = find_column_for_query(&self.schema, this_join_column)?;

        // 1. check if type is equal
        let this_col_type = self.schema.columns[this_col_index].col_type.raw_type();
        let that_col_type = inner_query.schema.columns[that_col_index].col_type.raw_type();

        if this_col_type != that_col_type {
            return Err(TableAccessError::LoadRowsError(format!("Join columns have different types: {} vs {}", this_col_type, that_col_type)));
        }
        
        let mut inner_table_hashes = HashMap::new();
        
        let inner_schema = inner_query.schema.clone();
        for (_, row) in inner_query.rows().into_iter() {
            let join_key = row.cells()[that_col_index].clone();
            inner_table_hashes.entry(join_key)
                .or_insert_with(Vec::new)
                .push(row);
        }

        let join_iter = self.row_iter.flat_map(move |(_, row)| {            
            let mut result = Vec::new();
            if let Some(join_tuples) = inner_table_hashes.get(&row.cells()[this_col_index]) {
                for inner_row in join_tuples {
                    let joined_cells: Vec<Cell> = row.cells().iter()
                    .chain(inner_row.cells().iter())
                    .cloned()
                    .collect();
                    result.push(Row::new(joined_cells));
                }
            }
            result.into_iter()            
        });

        let joined_cols: Vec<Column> = self.schema.columns
            .iter()
            .chain(inner_schema.columns.iter())
            .map(|col| (*col).clone())
            .collect();

        let joined_schema = TableSchema::new(joined_cols);

        Ok(QueryResult {
            row_iter: Box::new(join_iter),
            schema: joined_schema,
        })
    }
}

fn find_column_for_query(schema: &TableSchema, col_name: &str) -> Result<usize, TableAccessError> {
    let mut col_index = 0;
    let mut col_found = false;
    for (index, col) in schema.columns.iter().enumerate() {
        if col.name == col_name.trim() {
            col_index = index;
            col_found = true;
            break;
        }
    }

    if !col_found {
        return Err(TableAccessError::LoadRowsError(format!("Column '{}' not found!", col_name)));
    }

    Ok(col_index)
}

fn find_column_for_query_by_cell(schema: &TableSchema, col_name: &str, cell: &Cell) -> Result<usize, TableAccessError> {
    let col_index = find_column_for_query(schema, col_name)?;
    
    let ref_column = &schema.columns[col_index];
    if !cell.is_of_type(&ref_column.col_type) {
        return Err(TableAccessError::LoadRowsError(format!("Column '{}' is of type {} not {}", col_name, ref_column.col_type, cell.column_type())));
    }

    Ok(col_index)
}

impl From<PageError> for TableAccessError {
    fn from(err: PageError) -> Self {
        match err {
            PageError::InsertRowError => TableAccessError::InsertRowError("Failed to insert row into page.".to_string()),
            PageError::ReadPageError => TableAccessError::LoadRowsError("Failed to read page.".to_string()),
            PageError::UpdateRecordError => TableAccessError::LoadRowsError("Failed to update page.".to_string()),
        }
    }
}

impl From<RowValidationError> for TableAccessError {
    fn from(err: RowValidationError) -> Self {
        TableAccessError::InsertRowError(format!("Row validation error: {}", err))
    }
}

impl<'db, S: Store> TableAccess<'db, S> {
    pub fn with_indexes(mut self, indexed_columns: Vec<(i32, RefCell<BTreeStore>)>) -> Self {
        self.indexed_columns = indexed_columns;
        self
    }

    pub fn new(table: Table, store: &'db S, layout: &'db PageDataLayout) -> Self {
        Self { 
            table,
            store,
            layout,
            indexed_columns: Vec::new(),
            #[cfg(test)]
            index_used: RefCell::new(Vec::new()),
         }
    }

    fn fsm(&self) -> Fsm<'_, S> {
        Fsm::new(self.store, self.layout, &self.table)
    }

    /// Load all rows from all pages in the table
    pub fn find_all(&'db self) -> Result<QueryResult<'db, (Record, Row)>, TableAccessError> {
        let page_iter = self.store.seq_page_iterator(self.layout, &self.table)
            .map_err(|_| {
                TableAccessError::LoadRowsError("Cannot load PageIterator".to_owned())
            })?;
        Ok(QueryResult::new(page_iter, self.table.schema().clone()))
    }

    pub fn find(&'db self, col_name: &str, cell: Cell) -> Result<QueryResult<'db, (Record, Row)>, TableAccessError> {
        let col_index = find_column_for_query_by_cell(self.table.schema(), col_name, &cell)?;

        // check index for column index:
        let col_index_map = self.column_index_to_btree_pointer_map()?;
        if let Some(btree_pointer) = col_index_map.get(&col_index) {
            let val = cell.expect_int("Indexed values need to be of type Int")
                .map_err(|e| TableAccessError::LoadRowsError(e.to_string()))?;

            // This will later return an Vec<(i32, i32)> for non unique indexes
            let res = self.indexed_columns[*btree_pointer].1.borrow().find(val)
                .map_err(|e| TableAccessError::LoadRowsError(e.to_string()))?
                .map(|v| vec![v])
                .unwrap_or_default();

            let iter = IndexedRowIterator::new(&self.table, self.store, self.layout, res);
            let qr = QueryResult::from_indexes(iter, self.table.schema().clone());

            #[cfg(test)]
            self.index_used.borrow_mut().push(val);
        
            Ok(qr)
        } else {
            let page_iter = self.store.seq_page_iterator(self.layout, &self.table)
                .map_err(|_| {
                    TableAccessError::LoadRowsError("Cannot load PageIterator".to_owned())
                })?;
            let qr = QueryResult::new(page_iter, self.table.schema().clone());

            Ok(qr.filter(move |(_, row)| {
                row.cells()[col_index] == cell
            }))
        }
    }

    pub fn delete(&self, query_result: QueryResult<(Record, Row)>) -> Result<(), TableAccessError> {
        let mut page_row_map = HashMap::new();

        let col_index_btree_map = self.column_index_to_btree_pointer_map()?;

        for (record, row) in query_result.rows() {
            let delete_tuples = page_row_map.entry(*record.page_id()).or_insert(Vec::new());
            let mut uic = UpdateIndexCommand::new();

            for (col_idx, btree_idx) in col_index_btree_map.iter() {
                let val = row.cells()[*col_idx].expect_int("Indexed value must be of type Int")
                    .map_err(|e| TableAccessError::DeleteRowsError(e.to_string()))?;
                uic.push_delete((*btree_idx, val));
            }

            delete_tuples.push((record, uic));
        }

        for (page_id, records_to_delete) in page_row_map {
            for (record, uic) in records_to_delete {
                let mut page = self.store.read_page(self.layout, page_id, &self.table)
                    .map_err(|e| TableAccessError::DeleteRowsError(e.to_string()))?;

                page.delete_record(*record.record_index());
                self.update_index(page_id, *record.record_index(), uic)?;

                self.store.write_page(self.layout, &page, &self.table)
                    .map_err(|e| TableAccessError::DeleteRowsError(e.to_string()))?;

                self.fsm().update(&page);
            }
        }

        Ok(())
    }

    fn update_index(&self, page_id: i32, slot_id: usize, update_index_cmd: UpdateIndexCommand) -> Result<(), TableAccessError> {
        for (idx, old_val, new_val) in update_index_cmd.update_cells {
            if let Some(old_val) = old_val {
                self.indexed_columns[idx].1.borrow_mut().delete(old_val)
                    .map_err(|e| TableAccessError::UpdateRowsError(e.to_string()))?;
            }
            
            if let Some(new_val) = new_val {
                self.indexed_columns[idx].1.borrow_mut().insert(new_val, (page_id, slot_id as i32))
                    .map_err(|e| TableAccessError::UpdateRowsError(e.to_string()))?;
            }
        }

        Ok(())
    }

    fn column_index_to_btree_pointer_map(&self) -> Result<HashMap<usize, usize>, TableAccessError> {
        self.indexed_columns.iter()
            .enumerate()
            .map(|(btree_pointer, (col_id, _))| {
                let col_idx = self.table.schema().find_index_by_id(col_id)
                    .ok_or_else(|| TableAccessError::UpdateRowsError(format!("Cannot find columnd with id {} in schema", col_id)))?;
                Ok((col_idx, btree_pointer))
            }).collect::<Result<HashMap<usize, usize>, TableAccessError>>()
    } 

    pub fn update(&self, query_result: QueryResult<(Record, Row)>, updates: Vec<(&str, Cell)>) -> Result<(), TableAccessError> {
        if query_result.schema != *self.table.schema() {
            return Err(TableAccessError::UpdateRowsError("QueryResult schema does not match the schema of the table that is supposed to be updated".to_string()));
        }

        // Mapping column index (schema) to the index of the Vec where the BTree is located
        let index_to_btree_pointer_map = self.column_index_to_btree_pointer_map()?;
        // key: index of the column in the schema
        // value: column id and Cell with new value
        let new_values = updates.iter().map(|(col_name, cell)| {
            let index = find_column_for_query_by_cell(&self.table.schema(), col_name, &cell)?;
            Ok((index, cell.clone()))
        }).collect::<Result<HashMap<usize, Cell>, TableAccessError>>()?;

        let not_in_place_update = updates.iter().any(|(_, cell)| cell.column_type().is_var_size());

        // updated_rows_map are complete rows constructed of old values and the updated values
        // key is the 'page_id' of the current data
        // Better approach: instead of cloning everything, just replace the updated Cells in the existing Row. E.g, Row::replace(index, new_cell);
        let mut updated_rows_map: HashMap<i32, Vec<(Record, Row, UpdateIndexCommand)>> = HashMap::new();


        for (record, row) in query_result.rows() {
            let mut updated_cells = Vec::new();
            let mut index_update_cmd = UpdateIndexCommand::new();
            for (queried_cell_index, old_cell) in row.cells().iter().enumerate() {
                if let Some(updated_cell) = new_values.get(&queried_cell_index) {

                    if let Some(btree_pointer) = index_to_btree_pointer_map.get(&queried_cell_index) {
                        // is an indexed value
                        let old_value = old_cell.expect_int("Int expected for indexed values")
                            .map_err(|e| TableAccessError::UpdateRowsError(e.to_string()))?;
                        let new_value = updated_cell.expect_int("Int expected for indexed values")
                            .map_err(|e| TableAccessError::UpdateRowsError(e.to_string()))?;

                        index_update_cmd.push_update((*btree_pointer, old_value, new_value));
                    }

                    updated_cells.push(updated_cell.clone());
                } else {
                    updated_cells.push(old_cell.clone());
                }
            }
            let updated_row = Row::new(updated_cells);

            let updated_rows_per_page = updated_rows_map.entry(*record.page_id()).or_insert(Vec::new());
            updated_rows_per_page.push((record, updated_row, index_update_cmd));
        }

        let mut rows_needs_another_page = Vec::new();
        // iterate over updated_rows_map and write back updated rows to pages
        // in place or delete and reinsert
        for (page_id, updated_rows) in updated_rows_map.into_iter() {
            let mut page = self.store.read_page(self.layout, page_id, &self.table)
                .map_err(|e| TableAccessError::UpdateRowsError(e.to_string()))?;

            if not_in_place_update {
                // delete and reinsert
                for (record, updated_row, update_index_cmd) in updated_rows {
                    page.delete_record(*record.record_index());
                    let row_data = updated_row.serialize();
                    // If it's possible to insert into same page, than do it:
                    // if not insert into another page later
                    if page.can_insert(&row_data) {
                        let slot_id = page.insert_record(row_data)?;
                    
                        // Update index (before writing page, so that on error the age will not be written)
                        self.update_index(page.page_id(), slot_id, update_index_cmd)?;
                    } else {
                        rows_needs_another_page.push((row_data, update_index_cmd));
                    }

                    self.store.write_page(self.layout, &page, &self.table)
                        .map_err(|_| TableAccessError::UpdateRowsError("Reinsert update error: cannot write page".to_string()))?;
                    
                    self.fsm().update(&page);
                }
            } else {
                // easy peasy in place update
                for (record, updated_row, update_index_cmd) in updated_rows {
                    let row_data = updated_row.serialize();
                    page.write_record(*record.record_index(), row_data)?;
                    self.update_index(page.page_id(), *record.record_index(), update_index_cmd)?;

                    self.store.write_page(self.layout, &page, &self.table)
                        .map_err(|_| TableAccessError::UpdateRowsError("Write in place error: cannot write page".to_string()))?;

                    // the size of the page hasn't changed, so no fsm call needed
                }
            }
        }

        for (updated_row_data, update_index_cmd) in rows_needs_another_page {
            self.raw_insert(
                updated_row_data,
                move |s, (page_id, slot_id)| {
                    s.update_index(page_id, slot_id, update_index_cmd)
                }
            )?;
        }       
        
        Ok(())
    }

    // Uses FSM to find a page that has enough space
    /// Returns (page_id, slot_id)
    fn raw_insert<B: FnOnce(&Self, (i32, usize)) -> Result<(), TableAccessError>>(&self, row_data: Vec<u8>, before_saving_hook: B) -> Result<(i32, usize), TableAccessError> {
        let mut page = self.fsm().find_available_page(row_data.len());

        if !page.can_insert(&row_data) {
            return Err(TableAccessError::InsertRowError("Critical: FSM returned page with too little space. Likely a corruption of FSM.".to_owned()))
        }

        let slot_id = page.insert_record(row_data)?;

        before_saving_hook(self, (page.page_id(), slot_id))?;

        self.store.write_page(self.layout, &page, &self.table)
            .map_err(|e| TableAccessError::InsertRowError(format!("Cannot write page: {}", e.to_string())))?;

        self.fsm().update(&page);

        Ok((page.page_id(), slot_id))
    }

    pub fn insert(&self, row: &Row) -> Result<(), TableAccessError> {
        row.validate(self.table.schema())?;

        let col_index_btree_map = self.column_index_to_btree_pointer_map()?;

        let mut uic = UpdateIndexCommand::new();
        for (col_idx, btree_idx) in col_index_btree_map {
            let val = row.cells()[col_idx].expect_int("Indexed value must be of type Int")
                .map_err(|e| TableAccessError::InsertRowError(e.to_string()))?;
            uic.push_insert((btree_idx, val));
        }

        self.raw_insert(row.serialize(), move |s, (page_id, slot_id)| {
                    s.update_index(page_id, slot_id, uic)
        })?;
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use tempfile::tempdir;

    use crate::{data::page::PageDataLayout, database::table_access::TableAccess, fsm::fsm::Fsm, store::{IndexedRowIterator, Store, file_store::FileStore}, table::{Column, ColumnType, TableSchema, table::{Cell, Row, Table}}
    };

    #[test]
    fn should_delete_multiple() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Varchar(7)),
            Column::new(2, "someint", ColumnType::Int),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();
        
        let access = TableAccess::new(table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned()),
            Cell::Int(42),
        ]);
        let second = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned()),
            Cell::Int(52),
        ]);
        let third = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned()),
            Cell::Int(62),
        ]);
        let fourth = Row::new(vec![
            Cell::Varchar("Hare".to_owned()),
            Cell::Int(82),
        ]);

        access.insert(&first).unwrap();
        access.insert(&second).unwrap();
        access.insert(&third).unwrap();
        access.insert(&fourth).unwrap();

        // Act: DELETE FROM test WHERE value = "Rabbit"
        let result = access.find("value", Cell::Varchar("Rabbit".to_owned())).unwrap();

        access.delete(result).unwrap();

        // Assert: Only row with value = "Hare" should remain
        let result = access.find_all().unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.cells(), &[Cell::Varchar("Hare".to_owned()), Cell::Int(82)]);
    }

    #[test]
    fn should_update_multiple_with_delete_reinsert() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Varchar(7)),
            Column::new(2, "someint", ColumnType::Int),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let access = TableAccess::new(table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned()),
            Cell::Int(42),
        ]);
        let second = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned()),
            Cell::Int(52),
        ]);
        let third = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned()),
            Cell::Int(62),
        ]);
        let fourth = Row::new(vec![
            Cell::Varchar("Hare".to_owned()),
            Cell::Int(82),
        ]);

        access.insert(&first).unwrap();
        access.insert(&second).unwrap();
        access.insert(&third).unwrap();
        access.insert(&fourth).unwrap();

        // Act: UPDATE test SET value = "Bear" WHERE value = "Rabbit"
        let result = access.find("value", Cell::Varchar("Rabbit".to_owned())).unwrap();

        access.update(result, vec![("value", Cell::Varchar("Bear".to_owned()))]).unwrap();

        // Assert: row with value "Rabbit" shouldn't exist anymore
        let result = access.find("value", Cell::Varchar("Rabbit".to_owned())).unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 0);

        // Assert: row with value "Bear" exists instead
        let result = access.find("value", Cell::Varchar("Bear".to_owned())).unwrap();
        let rows = result.rows();

        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn should_update_with_delete_reinsert() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Varchar(7)),
            Column::new(2, "someint", ColumnType::Int),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let access = TableAccess::new(table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned()),
            Cell::Int(42),
        ]);
        let second_row = Row::new(vec![
            Cell::Varchar("Hare".to_owned()),
            Cell::Int(82),
        ]);

        access.insert(&first_row).unwrap();
        access.insert(&second_row).unwrap();

        // Act: UPDATE test SET value = "Bear" WHERE value = "Rabbit"
        let result = access.find("value", Cell::Varchar("Rabbit".to_owned())).unwrap();
        access.update(result, vec![("value", Cell::Varchar("Bear".to_owned()))]).unwrap();

        // Assert: row with value "Rabbit" shouldn't exist anymore
        let result = access.find("value", Cell::Varchar("Rabbit".to_owned())).unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 0);

        // Assert: row with value "Bear" exists instead
        let result = access.find("value", Cell::Varchar("Bear".to_owned())).unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn should_ensure_unique_values() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Int),
            Column::new(2, "byte", ColumnType::Byte),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let btree = RefCell::new(store.read_btree(1).unwrap());

        let access = TableAccess::new(table.clone(), &store, &layout)
            .with_indexes(vec![(1, btree)]);

        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(42),
            Cell::Byte(1),
        ]);
        let second_row = Row::new(vec![
            Cell::Int(42),
            Cell::Byte(2),
        ]);

        access.insert(&first_row).unwrap();

        // insert second row should fail
        let res = access.insert(&second_row);
        assert!(res.is_err());
        let err = res.unwrap_err();
        assert!(err.to_string().contains("Unique key constraint"));

        let rows = access.find_all().unwrap().rows();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn should_insert_using_index() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Int),
            Column::new(2, "byte", ColumnType::Byte),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let btree = RefCell::new(store.read_btree(1).unwrap());

        let access = TableAccess::new(table.clone(), &store, &layout)
            .with_indexes(vec![(1, btree)]);

        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(42),
            Cell::Byte(1),
        ]);

        access.insert(&first_row).unwrap();

        // Assert: BTree has stored the correct location of the value
        let btree = store.read_btree(1).unwrap();
        let result = btree.find(42).unwrap();
        assert!(result.is_some());
        let (page_id, slot_id) = result.unwrap();
        let page = store.read_page(&layout, page_id, &table).unwrap();

        let row = page.read_slot(slot_id as usize)
            .map(|data| Row::deserialize(data, table.schema())).unwrap();

        let cells = row.cells();
        assert_eq!(cells, &vec![Cell::Int(42), Cell::Byte(1)]);
    }

    #[test]
    fn should_find_using_index() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Int),
            Column::new(2, "byte", ColumnType::Byte),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let btree = RefCell::new(store.read_btree(1).unwrap());

        let access = TableAccess::new(table.clone(), &store, &layout)
            .with_indexes(vec![(1, btree)]);

        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(42),
            Cell::Byte(1),
        ]);

        access.insert(&first_row).unwrap();
        let query = access.find("value", Cell::Int(42)).unwrap();
        let indexes_used = access.index_used.borrow();
        assert_eq!(*indexes_used, vec![42]);

        let rows = query.rows();
        let cells = rows[0].1.cells();

        assert_eq!(
            cells,
            &vec![
                Cell::Int(42),
                Cell::Byte(1),
            ]  
        );

    }

    #[test]
    fn should_use_index_on_delete() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Int),
            Column::new(2, "byte", ColumnType::Byte),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let btree = RefCell::new(store.read_btree(1).unwrap());

        let access = TableAccess::new(table.clone(), &store, &layout)
            .with_indexes(vec![(1, btree)]);

        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(42),
            Cell::Byte(1),
        ]);

        access.insert(&first_row).unwrap();
        let query = access.find("value", Cell::Int(42)).unwrap();
        access.delete(query).unwrap();

        let btree = store.read_btree(1).unwrap();
        let result = btree.find(42).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn should_update_and_use_index() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Int),
            Column::new(2, "byte", ColumnType::Byte),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        // Small page size, so we will have 2 pages (14 bytes header, 5 bytes row + 7 bytes slot size)
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let btree = RefCell::new(store.read_btree(1).unwrap());

        let access = TableAccess::new(table.clone(), &store, &layout)
            .with_indexes(vec![(1, btree)]);

        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(42),
            Cell::Byte(1),
        ]);
        let second_row = Row::new(vec![
            Cell::Int(82),
            Cell::Byte(2),
        ]);

        access.insert(&first_row).unwrap();
        access.insert(&second_row).unwrap();

        // Act: UPDATE test SET value = 99 WHERE value = 82
        let result = access.find("value", Cell::Int(82)).unwrap();
        access.update(result, vec![("value", Cell::Int(99))]).unwrap();


        // Assert: BTree has stored the correct location of the value
        let btree = store.read_btree(1).unwrap();
        let result = btree.find(99).unwrap();
        assert!(result.is_some());
        // Vector, because in future indexes with multiple values may exist:
        let indexes = vec![result.unwrap()];
        let mut iter = IndexedRowIterator::new(&table, &store, &layout, indexes);

        let next = iter.next();
        assert!(next.is_some());
        let next_row = next.unwrap();
        let cells = next_row.1.cells();
        assert_eq!(cells, &vec![Cell::Int(99), Cell::Byte(2)]);
        assert!(iter.next().is_none());
    }

    #[test]
    fn should_update_in_place_rows() {
        let schema = TableSchema::new(vec![
            Column::new(1, "value", ColumnType::Int),
            Column::new(2, "byte", ColumnType::Byte),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        // Small page size, so we will have 2 pages (14 bytes header, 5 bytes row + 7 bytes slot size)
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let access = TableAccess::new(table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(42),
            Cell::Byte(1),
        ]);
        let second_row = Row::new(vec![
            Cell::Int(82),
            Cell::Byte(2),
        ]);

        access.insert(&first_row).unwrap();
        access.insert(&second_row).unwrap();

        // Act: UPDATE test SET value = 99 WHERE value = 82
        let result = access.find("value", Cell::Int(82)).unwrap();
        access.update(result, vec![("value", Cell::Int(99))]).unwrap();

        // Assert: row with value 88 shouldn't exist anymore
        let result = access.find("value", Cell::Int(82)).unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 0);

        // Assert: row with value 99 exists instead
        let result = access.find("value", Cell::Int(99)).unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 1);
    }


    #[test]
    fn should_insert_two_rows() {
        let schema = TableSchema::new(vec![
            Column::new(1, "name", ColumnType::Varchar(10))
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        store.create(&layout, &table).unwrap();

        let access = TableAccess::new(table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Varchar("Hans".to_owned())
        ]);
        let second_row = Row::new(vec![
            Cell::Varchar("Rabbit".to_owned())
        ]);

        access.insert(&first_row).unwrap();
        access.insert(&second_row).unwrap();

        let result = access.find_all().unwrap();
        let rows = result.rows().into_iter().map(|(_, row)| row).collect::<Vec<Row>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cells(), &[Cell::Varchar("Hans".to_owned())]);
        assert_eq!(rows[1].cells(), &[Cell::Varchar("Rabbit".to_owned())]);
    }

    #[test]
    fn should_find_a_row() {
        let schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int),
            Column::new(2, "name", ColumnType::Varchar(10))
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();

        store.create(&layout, &table).unwrap();

        let access = TableAccess::new(table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(1),
            Cell::Varchar("Hans".to_owned())
        ]);

        let second_row = Row::new(vec![
            Cell::Int(2),
            Cell::Varchar("Rabbit".to_owned())
        ]);

        access.insert(&first_row).unwrap();
        access.insert(&second_row).unwrap();

        let result = access.find("name", Cell::Varchar("Hans".to_owned())).unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 1);
        let row = rows.get(0).unwrap();
        assert!(matches!(row.1.cells().as_slice(), [Cell::Int(id), Cell::Varchar(name)] if *id == 1 && name == "Hans"));
    }

     #[test]
    fn should_find_multiple_rows() {
        let schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int),
            Column::new(2, "name", ColumnType::Varchar(10))
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();
        
        store.create(&layout, &table).unwrap();

        let access = TableAccess::new(table.clone(), &store, &layout);

        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(1),
            Cell::Varchar("Hans".to_owned())
        ]);

        let second_row = Row::new(vec![
            Cell::Int(2),
            Cell::Varchar("Hans".to_owned())
        ]);

        access.insert(&first_row).unwrap();
        access.insert(&second_row).unwrap();

        let result = access.find("name", Cell::Varchar("Hans".to_owned())).unwrap();
        let rows = result.rows();
        assert_eq!(rows.len(), 2);
        let row = rows.get(0).unwrap();
        assert!(matches!(row.1.cells().as_slice(), [Cell::Int(id), Cell::Varchar(name)] if *id == 1 && name == "Hans"));

        let row = rows.get(1).unwrap();
        assert!(matches!(row.1.cells().as_slice(), [Cell::Int(id), Cell::Varchar(name)] if *id == 2 && name == "Hans"));
    }

    #[test]
    fn find_should_return_error_if_cell_has_wrong_type() {
        let schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int),
            Column::new(2, "name", ColumnType::Varchar(10))
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout = PageDataLayout::new(256).unwrap();

        store.create(&layout, &table).unwrap();
        let access = TableAccess::new(table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&table);
        store.create(&layout, &fsm_access).unwrap();

        let first_row = Row::new(vec![
            Cell::Int(1),
            Cell::Varchar("Hans".to_owned())
        ]);

        let second_row = Row::new(vec![
            Cell::Int(2),
            Cell::Varchar("Rabbit".to_owned())
        ]);

        access.insert(&first_row).unwrap();
        access.insert(&second_row).unwrap();

        let rows = access.find("name", Cell::Int(1));
        assert!(rows.is_err());
        let err_message = rows.err().unwrap().to_string();
        assert!(err_message.contains("Column 'name' is of type Varchar not Int"));
    }

    #[test]
    fn should_join_two_tables() {
        let person_schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int),
            Column::new(2, "name", ColumnType::Varchar(10))
        ]);

        let person_table = Table::new(1, "persons".to_owned(), person_schema);
        let base_dir = tempdir().unwrap();
        let store = FileStore::new(base_dir.path());
        let layout: PageDataLayout = PageDataLayout::new(1024).unwrap();

        store.create(&layout, &person_table).unwrap();
        let person_access = TableAccess::new(person_table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&person_table);
        store.create(&layout, &fsm_access).unwrap();

        let first_person = Row::new(vec![
            Cell::Int(1),
            Cell::Varchar("Hans".to_owned())
        ]);

        let second_person = Row::new(vec![
            Cell::Int(2),
            Cell::Varchar("Rabbit".to_owned())
        ]);

        person_access.insert(&first_person).unwrap();
        person_access.insert(&second_person).unwrap();

        let address_schema = TableSchema::new(vec![
            Column::new(1, "person_id", ColumnType::Int),
            Column::new(2, "address", ColumnType::Varchar(64))
        ]);

        let address_table = Table::new(2, "addresses".to_owned(), address_schema);
        store.create(&layout, &address_table).unwrap();

        let address_access = TableAccess::new(address_table.clone(), &store, &layout);
        let fsm_access = Fsm::access(&address_table);
        store.create(&layout, &fsm_access).unwrap();

        let first_address = Row::new(vec![
            Cell::Int(1),
            Cell::Varchar("Lilienstr 99".to_owned())
        ]);

        let second_address = Row::new(vec![
            Cell::Int(2),
            Cell::Varchar("Bergmansweg 10".to_owned())
        ]);

        address_access.insert(&first_address).unwrap();
        address_access.insert(&second_address).unwrap();

        // ACT:
        // SELECT * FROM person 
        // JOIN address ON person.id = address.person_id
        let person_result = person_access.find_all().unwrap();
        let address_result = address_access.find_all().unwrap();

        let result = person_result.hash_join(address_result, "id", "person_id").unwrap();
        assert_eq!(result.schema.columns.len(), 4);
        assert_eq!(result.schema.columns[0], Column::new(1, "id", ColumnType::Int));
        assert_eq!(result.schema.columns[1], Column::new(2, "name", ColumnType::Varchar(10)));
        assert_eq!(result.schema.columns[2], Column::new(1, "person_id", ColumnType::Int));
        assert_eq!(result.schema.columns[3], Column::new(2, "address", ColumnType::Varchar(64)));

        let rows = result.rows();
        assert_eq!(rows.len(), 2);
        let cells_hans = rows[0].cells();
        assert_eq!(
            *cells_hans, 
            vec![Cell::Int(1), Cell::Varchar("Hans".to_owned()), Cell::Int(1), Cell::Varchar("Lilienstr 99".to_owned())]);

        let cells_rabbit = rows[1].cells();
        assert_eq!(
            *cells_rabbit, 
            vec![Cell::Int(2), Cell::Varchar("Rabbit".to_owned()), Cell::Int(2), Cell::Varchar("Bergmansweg 10".to_owned())]);
    }

}