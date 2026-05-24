use std::{
    fs::{File, remove_file},
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use crate::{
    data::page::{Page, PageDataLayout, PageFileMetadata},
    store::{PageStorage, Store, StoreError},
    tree::store::BTreeStore,
};

// Defines how many keys fit into one node
const BTREE_MAX_DEGREE: u16 = 500;

pub struct FileStore {
    base_path: PathBuf,
}
impl FileStore {
    pub fn new(base_path: &Path) -> Result<Self, StoreError> {
        if !base_path.is_dir() {
            return Err(StoreError::InvalidBasePath(format!(
                "FileStore needs a directory as a base_path: {}",
                base_path.display()
            )));
        }
        Ok(Self {
            base_path: base_path.to_path_buf(),
        })
    }

    fn file_path(&self, page_storage: &dyn PageStorage) -> PathBuf {
        self.base_path.join(page_storage.file_path())
    }

    fn delete_file(&self, page_storage: &dyn PageStorage) -> Result<(), StoreError> {
        remove_file(self.file_path(page_storage))?;
        Ok(())
    }

    fn init(
        &self,
        layout: &PageDataLayout,
        page_storage: &dyn PageStorage,
    ) -> Result<(), StoreError> {
        let metadata = PageFileMetadata::new();
        self.write_metadata(layout, &metadata, page_storage)
    }

    fn write_metadata(
        &self,
        layout: &PageDataLayout,
        metadata: &PageFileMetadata,
        page_storage: &dyn PageStorage,
    ) -> Result<(), StoreError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.file_path(page_storage))?;

        file.write_all(&metadata.serialize(layout))?;

        Ok(())
    }
}

impl Store for FileStore {
    type I<'database>
        = FilePageIterator<'database, Self>
    where
        Self: 'database;

    fn delete_all(&self) -> Result<(), StoreError> {
        for entry in std::fs::read_dir(&self.base_path)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                println!("Deleting file: {:?}", path);
                // std::fs::remove_file(path)?;
            }
        }
        Ok(())
    }

    fn read_metadata(
        &self,
        layout: &PageDataLayout,
        page_storage: &dyn PageStorage,
    ) -> Result<PageFileMetadata, StoreError> {
        let path: PathBuf = self.file_path(page_storage);
        if !path.exists() {
            return Err(StoreError::InvalidState(format!(
                "No such data structure '{}' found (forget to call create?)",
                page_storage.file_path()
            )));
        }

        let mut file = std::fs::OpenOptions::new().read(true).open(path)?;

        let fmeta = file.metadata()?;
        if fmeta.len() < layout.metadata_size() as u64 {
            return Err(StoreError::InvalidBasePath(
                "File size is smaller than expected metadata size".to_string(),
            ));
        }

        let mut buf = vec![0u8; layout.metadata_size()];
        file.read_exact(&mut buf)?;

        Ok(PageFileMetadata::deserialize(&buf)?)
    }

    fn read_page<'database>(
        &self,
        layout: &'database PageDataLayout,
        page_id: i32,
        page_storage: &dyn PageStorage,
    ) -> Result<Page<'database>, StoreError> {
        if page_id < 1 {
            return Err(StoreError::InvalidState(format!(
                "Invalid page_id ({}). page_id must be positive value and not 0",
                page_id
            )));
        }

        let mut page_data = vec![0; layout.page_size()];

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(self.base_path.join(page_storage.file_path()))?;

        let page_pos = page_id - 1;

        file.seek(SeekFrom::Start(
            (layout.metadata_size() + page_pos as usize * layout.page_size()) as u64,
        ))?;

        file.read_exact(&mut page_data)?;

        let p = Page::deserialize(&page_data, layout)?;
        Ok(p)
    }

    fn write_page(
        &self,
        layout: &PageDataLayout,
        page: &Page,
        page_storage: &dyn PageStorage,
    ) -> Result<(), StoreError> {
        let data = page.serialize();

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.base_path.join(page_storage.file_path()))?;
        let page_pos = page.page_id() - 1;
        file.seek(SeekFrom::Start(
            (layout.metadata_size() + page_pos as usize * layout.page_size()) as u64,
        ))?;
        file.write_all(&data)?;
        Ok(())
    }

    fn allocate_page<'database>(
        &self,
        layout: &'database PageDataLayout,
        page_storage: &dyn PageStorage,
    ) -> Result<Page<'database>, StoreError> {
        let mut metadata = self.read_metadata(layout, page_storage)?;
        let mut new_page = Page::new(layout);
        new_page.set_page_id(metadata.allocate_next_page_id());

        // ToDo: here we can get into an inconsistent state if write_page fails after write_metadata succeeded
        self.write_metadata(layout, &metadata, page_storage)?;
        self.write_page(layout, &new_page, page_storage)?;
        Ok(new_page)
    }

    fn seq_page_iterator<'database>(
        &'database self,
        layout: &'database PageDataLayout,
        page_storage: &'database dyn PageStorage,
    ) -> Result<FilePageIterator<'database, Self>, StoreError>
    where
        Self: Sized,
    {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(self.base_path.join(page_storage.file_path()))?;

        file.seek(SeekFrom::Start(layout.metadata_size() as u64))?;

        let reader = BufReader::new(file);

        Ok(FilePageIterator::new(page_storage, self, layout)?.with_reader(reader))
    }

    fn create(
        &self,
        layout: &PageDataLayout,
        page_storage: &dyn PageStorage,
    ) -> Result<(), StoreError> {
        if std::fs::exists(self.file_path(page_storage))? {
            return Err(StoreError::InvalidState(format!(
                "Data structure '{}' already exists",
                page_storage.file_path()
            )));
        }
        std::fs::File::create(self.file_path(page_storage))?;
        self.init(layout, page_storage)
    }

    fn delete(&self, page_storage: &dyn PageStorage) -> Result<(), StoreError> {
        self.delete_file(page_storage)
    }

    fn read_btree(&self, btree_id: i32) -> Result<BTreeStore, StoreError> {
        let index_file = format!("btreeindex_{}.dat", btree_id);
        let full_path = self.base_path.join(index_file);
        Ok(BTreeStore::new(&full_path, BTREE_MAX_DEGREE)?)
    }
}

pub struct FilePageIterator<'db, S: Store> {
    layout: &'db PageDataLayout,
    store: &'db S,
    reader: Option<BufReader<File>>,
    table: &'db dyn PageStorage,
    current_page_id: i32,
    total_pages: i32,
}

impl<'db, S: Store> FilePageIterator<'db, S> {
    pub fn new(
        table: &'db dyn PageStorage,
        store: &'db S,
        layout: &'db PageDataLayout,
    ) -> Result<Self, StoreError> {
        let metadata = store.read_metadata(layout, table)?;
        let total_pages = metadata.number_of_pages();
        Ok(Self {
            table,
            layout,
            store,
            reader: None,
            current_page_id: 1,
            total_pages,
        })
    }

    pub fn with_reader(mut self, reader: BufReader<File>) -> Self {
        self.reader = Some(reader);
        self
    }
}

impl<'db, S: Store> Iterator for FilePageIterator<'db, S> {
    type Item = Result<Page<'db>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current_page_id > self.total_pages {
            return None;
        }

        let mut buf = vec![0u8; self.layout.page_size()];
        let page = if let Some(reader) = self.reader.as_mut() {
            reader
                .read_exact(&mut buf)
                .map_err(StoreError::from)
                .and_then(|_| Page::deserialize(&buf, self.layout).map_err(StoreError::from))
        } else {
            self.store
                .read_page(self.layout, self.current_page_id, self.table)
        };

        self.current_page_id += 1;

        Some(page)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{
        data::page::PageDataLayout,
        schema::{
            Column, ColumnType, TableSchema,
            table::{Cell, Row, Table},
        },
        store::{
            Store,
            file_store::{FilePageIterator, FileStore},
        },
    };

    struct Sequence {
        col_id: i32,
        current: i32,
    }

    impl Sequence {
        fn serialize(&self) -> Vec<u8> {
            let mut buf = [0; 8];
            buf[0..4].copy_from_slice(&self.col_id.to_be_bytes());
            buf[4..8].copy_from_slice(&self.current.to_be_bytes());
            buf.to_vec()
        }

        fn deserialize(data: &[u8]) -> Self {
            let col_id = i32::from_be_bytes(data[0..4].try_into().unwrap());
            let current = i32::from_be_bytes(data[4..8].try_into().unwrap());

            Self { col_id, current }
        }
    }

    #[test]
    fn should_allocate_and_write_page() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).unwrap();

        let layout = PageDataLayout::new(128).unwrap();

        let schema = TableSchema::new(vec![Column::new(1, "id", ColumnType::Int)]);

        let table = Table::new(1, "test".to_owned(), schema);

        store.create(&layout, &table).unwrap();
        let mut new_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(new_page.page_id(), 1);

        let row = Row::new(vec![Cell::Int(42)]);

        new_page.insert_record(row.serialize()).unwrap();

        store.write_page(&layout, &new_page, &table).unwrap();

        let loaded_page = store.read_page(&layout, 1, &table).unwrap();

        let row = Row::deserialize(loaded_page.row_data(), table.schema()).unwrap();

        assert_eq!(row.cells().len(), 1);
        matches!(row.cells().get(0).unwrap(), Cell::Int(42));
    }

    #[test]
    fn should_can_allocate_twice() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).unwrap();

        let layout = PageDataLayout::new(128).unwrap();

        let schema = TableSchema::new(vec![Column::new(1, "id", ColumnType::Int)]);

        let table = Table::new(1, "test".to_owned(), schema);

        store.create(&layout, &table).unwrap();
        // Create first page (stays empty)
        let first_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(first_page.page_id(), 1);

        store.write_page(&layout, &first_page, &table).unwrap();

        // Create second page with a row
        let mut second_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(second_page.page_id(), 2);

        let row = Row::new(vec![Cell::Int(42)]);

        second_page.insert_record(row.serialize()).unwrap();
        store.write_page(&layout, &second_page, &table).unwrap();
        let loaded_page = store.read_page(&layout, 2, &table).unwrap();

        let row = Row::deserialize(loaded_page.row_data(), table.schema()).unwrap();

        assert_eq!(row.cells().len(), 1);
        matches!(row.cells().get(0).unwrap(), Cell::Int(42));
    }

    #[test]
    fn should_be_able_to_store_arbitrary_data() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).unwrap();

        let layout = PageDataLayout::new(128).unwrap();

        let schema = TableSchema::new(vec![Column::new(1, "id", ColumnType::Int)]);
        let table = Table::new(1, "test".to_owned(), schema);

        store.create(&layout, &table).unwrap();
        // Create page for sequences
        let mut seq_page = store.allocate_page(&layout, &table).unwrap();

        let seq = Sequence {
            col_id: 1,
            current: 3,
        };

        seq_page.insert_record(seq.serialize()).unwrap();
        store.write_page(&layout, &seq_page, &table).unwrap();

        let loaded_page = store.read_page(&layout, 1, &table).unwrap();

        let seq_loaded: Sequence = Sequence::deserialize(loaded_page.row_data());

        assert_eq!(seq_loaded.col_id, 1);
        assert_eq!(seq_loaded.current, 3);
    }

    #[test]
    fn should_iterate_over_pages() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path()).unwrap();

        let layout = PageDataLayout::new(32).unwrap();

        let schema = TableSchema::new(vec![Column::new(1, "id", ColumnType::Int)]);

        let table = Table::new(1, "test".to_owned(), schema);

        store.create(&layout, &table).unwrap();
        let mut new_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(new_page.page_id(), 1);

        let row = Row::new(vec![Cell::Int(42)]);

        new_page.insert_record(row.serialize()).unwrap();
        store.write_page(&layout, &new_page, &table).unwrap();

        let mut iter = FilePageIterator::new(&table, &store, &layout).unwrap();

        let page = iter.next().unwrap().unwrap();

        assert_eq!(page.page_id(), 1);
        matches!(page.data_offset(), 28);
    }
}
