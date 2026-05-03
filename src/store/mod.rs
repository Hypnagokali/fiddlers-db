pub mod file_store;

use std::collections::HashMap;

use thiserror::Error;

use crate::{data::page::{Page, PageDataLayout, PageError, PageFileMetadata, Record, RecordIterator}, table::{TableSchema, table::{Row, Table}}, tree::store::{BTreeStore, BTreeStoreError}};

// Store is always owned by a Database instance
// ToDo:
//  - Get rid of the extra PageFileMetadata 
//    (number_of_pages could go into the catalog table and page_id is just the physical location in the file)
//  - Table lifecycle should be separated from page management (create, delete)
//  - Separate different concerns: raw physical layer, table related stuff (reading btree)
//  - of course, the methods should not depend on Table, because theoretically the store can store everything
pub trait Store {
    type I<'db>: Iterator<Item = Page<'db>> 
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
    type Item = (Record, Row);

    fn next(&mut self) -> Option<Self::Item> {
        let mut res = None;

        // Using loop instead, would avoid double checking is_none()
        while res.is_none() {
            if let Some(record_iter) = self.record_iter.as_mut() {
                res = record_iter.next().map(|r| {
                    let row = Row::deserialize(r.data(), self.table.schema());
                    (r, row)
                });

                if res.is_none() {
                    self.record_iter = None;
                }

            } else {
                let next = self.indexes.pop();
                if let Some((page_id, slots)) = next {
                    // Refactor unwrap here and in PageIterator as well
                    let page = self.store.read_page(self.layout, page_id, self.table).unwrap();
                    self.record_iter = Some(RecordIterator::from_slots(page, slots));
                } else {
                    return None;
                }
            }
        }

        res
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
    type Item = (Record, Row);

    fn next(&mut self) -> Option<Self::Item> {
        self.record_iterator.next()
            .map(|r| {
                let row = Row::deserialize(r.data(), &self.schema);

                (r, row)
            })
    }
}

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("StoreError - I/O Error: {0}")]
    IoError(String),
    #[error("StoreError - Deserialization Error: {0}")]
    DeserializationError(String),
    #[error("StoreError - Cannot read BTreeStore: {0}")]
    ReadBTreeStoreError(String),
}

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        StoreError::IoError(err.to_string())
    }
}

impl From<BTreeStoreError> for StoreError {
    fn from(err: BTreeStoreError) -> Self {
        StoreError::ReadBTreeStoreError(err.to_string())
    }
}

impl From<PageError> for StoreError {
    fn from(err: PageError) -> Self {
        StoreError::IoError(err.to_string())
    }
}
