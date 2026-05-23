pub mod file_store;

use std::collections::HashMap;

use thiserror::Error;

use crate::{data::page::{Page, PageDataLayout, PageError, PageFileMetadata, ReadMetadataError, Record, RecordIterator}, schema::{TableSchema, table::{Row, RowDeserializationError, Table}}, tree::store::{BTreeStore, BTreeStoreError}};

// Store is always owned by a Database instance
// ToDo:
//  - Get rid of the extra PageFileMetadata 
//    (number_of_pages could go into the catalog table and page_id is just the physical location in the file)
//  - Table lifecycle should be separated from page management (create, delete)
//  - Separate different concerns: raw physical layer, table related stuff (reading btree)
//  - of course, the methods should not depend on Table, because theoretically the store can store everything
pub trait Store {
    type I<'db>: Iterator<Item = Result<Page<'db>, StoreError>>
    where
        Self: 'db;
    // This method is just a very quick solution. If the Store will still provide access to the BTree in future,
    // the BTree output must be a trait to provide different implementations. BTreeStore is only file based at the moment.
    fn read_btree(&self, btree_id: i32) -> Result<BTreeStore, StoreError>;
    fn delete_all(&self) -> Result<(), StoreError>;
    fn create(&self, layout: &PageDataLayout, page_storage: &dyn PageStorage) -> Result<(), StoreError>;
    fn delete(&self, page_storage: &dyn PageStorage) -> Result<(), StoreError>;
    fn read_metadata(&self, layout: &PageDataLayout, page_storage: &dyn PageStorage) -> Result<PageFileMetadata, StoreError>;
    fn read_page<'db>(&self, layout: &'db PageDataLayout, page_id: i32, page_storage: &dyn PageStorage) -> Result<Page<'db>, StoreError>;
    fn write_page(&self, layout: &PageDataLayout, page: &Page, page_storage: &dyn PageStorage) -> Result<(), StoreError>;
    fn allocate_page<'db>(&self, layout: &'db PageDataLayout, page_storage: &dyn PageStorage) -> Result<Page<'db>, StoreError>;
    fn seq_page_iterator<'db>(&'db self, layout: &'db PageDataLayout, page_storage: &'db dyn PageStorage) 
        -> Result<Self::I<'db>, StoreError>;
}

// Just the path where the physical page lives
// PageStorage should perhaps be defined in a global scope
pub trait PageStorage {
    fn file_path(&self) -> String; 
}

impl PageStorage for Table {
    fn file_path(&self) -> String {
        format!("table_{}.dat", self.id())
    }
}

pub struct IndexedRowIterator<'db, S: Store> {
    layout: &'db PageDataLayout,
    store: &'db S,
    table: &'db Table,
    indexes: Vec<(i32, Vec<usize>)>, // page_id => Vec<slot_id>
    record_iter: Option<RecordIterator>,
    current_index: usize,
}

impl<'db, S: Store> IndexedRowIterator<'db, S> {
    pub fn new(table: &'db Table, store: &'db S, layout: &'db PageDataLayout, indexes: Vec<(i32, i32)>) -> Self {
        let mut map: HashMap<i32, Vec<usize>> = HashMap::new();

        for (page_id, slot_id) in indexes {
            map.entry(page_id).or_default().push(slot_id as usize);
        }

        let mut index_vec = Vec::new();
        for (page_id, slots) in map {
            index_vec.push((page_id, slots));
        }

        Self {
            table,
            layout,
            store,
            indexes: index_vec,
            record_iter: None,
            current_index: 0,
        }
    }
}

impl<'db, S: Store> Iterator for IndexedRowIterator<'db, S> {
    type Item = Result<(Record, Row), StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(record_iter) = self.record_iter.as_mut() {
                if let Some(r) = record_iter.next() {
                    return Some(Row::deserialize(r.data(), self.table.schema())
                        .map(|row| (r, row))
                        .map_err(StoreError::from));
                }

                self.record_iter = None;
                continue;
            }

            if let Some((page_id, slots)) = self.indexes.pop() {
                match self.store.read_page(self.layout, page_id, self.table) {
                    Ok(page) => {
                        self.record_iter = Some(RecordIterator::from_slots(page, slots));
                    }
                    Err(err) => return Some(Err(err)),
                }
            } else {
                return None;
            }
        }
    }
}

pub struct PageRowIterator {
    record_iterator: RecordIterator,
    schema: TableSchema,
}

impl PageRowIterator {
    pub fn new(page: Page, schema: TableSchema) -> Self {
        Self { 
            record_iterator: page.record_iterator(),
            schema 
        }
    }
}

impl Iterator for PageRowIterator {
    // Record is needed for accessing a slot directly (e.g., when we want to delete a row)
    type Item = Result<(Record, Row), StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.record_iterator.next()
            .map(|r| Row::deserialize(r.data(), &self.schema)
                .map(|row| (r, row))
                .map_err(StoreError::from))
    }
}

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("Store I/O error")]
    Io(#[from] std::io::Error),
    #[error("Store page error")]
    Page(#[from] PageError),
    #[error("Store metadata error")]
    Metadata(#[from] ReadMetadataError),
    #[error("Store row deserialization error")]
    RowDeserialization(#[from] RowDeserializationError),
    #[error("Store B-Tree error")]
    BTree(#[from] BTreeStoreError),
    #[error("Invalid store base path: {0}")]
    InvalidBasePath(String),
    #[error("Invalid store state: {0}")]
    InvalidState(String),
}
