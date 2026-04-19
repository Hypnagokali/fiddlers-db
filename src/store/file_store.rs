use std::{fs::remove_file, io::{Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}};

use crate::{data::page::{Page, PageDataLayout, PageFileMetadata}, store::{PageStorage, Store, StoreError}, tree::store::BTreeStore};

// Defines how many keys fit into one node
const BTREE_MAX_DEGREE: u16 = 500;

pub struct FileStore {
    base_path: PathBuf,
}
impl FileStore {
    pub fn new(base_path: &Path) -> Self {
        if !base_path.is_dir() {
            // TODO: Use proper error handling
            panic!("FileStore needs a directory as a base_path");
        }
        Self { 
            base_path: base_path.to_path_buf(),
         }
    }

    fn file_path(&self, page_storage: &dyn PageStorage) -> PathBuf {
        self.base_path.join(page_storage.file_path())
    }

    fn delete_file(&self, page_storage: &dyn PageStorage) -> Result<(), StoreError> {
        remove_file(self.file_path(page_storage))
            .map_err(|e| StoreError::IoError(e.to_string()))?;

        Ok(())
    }

    fn init(&self, layout: &PageDataLayout, page_storage: &dyn PageStorage) -> Result<(), StoreError> {
        let metadata = PageFileMetadata::new();
        self.write_metadata(layout, &metadata, page_storage)
    }

    fn write_metadata(&self, layout: &PageDataLayout, metadata: &PageFileMetadata, page_storage: &dyn PageStorage) -> Result<(), StoreError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.file_path(page_storage))?;

        file.write_all(&metadata.serialize(layout))?;

        Ok(())
    }
}
impl Store for FileStore {
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

    fn read_metadata(&self, layout: &PageDataLayout, page_storage: &dyn PageStorage) -> Result<PageFileMetadata, StoreError> {
        let path: PathBuf = self.file_path(page_storage);
        if !path.exists() {
            return Err(StoreError::IoError(format!("No such data structure '{}' found (forget to call create?)", page_storage.file_path())));
        }

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(path)?;

        let fmeta = file.metadata().unwrap();
        if fmeta.len() < layout.metadata_size() as u64 {
            return Err(StoreError::IoError("Metadata size is smaller than expected".to_string()));
        }

        let mut buf = vec![0u8; layout.metadata_size()];
        file.read_exact(&mut buf)?;

        Ok(PageFileMetadata::deserialize(&buf))
    }

    fn read_page<'database>(&self, layout: &'database PageDataLayout, page_id: i32, page_storage: &dyn PageStorage) -> Result<Page<'database>, StoreError> {
        let mut page_data = vec![0; layout.page_size()];

        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .open(self.base_path.join(page_storage.file_path()))?;

        let page_pos = page_id - 1;
        file.seek(SeekFrom::Start((layout.metadata_size() + page_pos as usize * layout.page_size()) as u64))?;
    
        file.read_exact(&mut page_data)?;

        let p = Page::deserialize(&page_data, layout);
        Ok(p)
    }

    fn write_page(&self, layout: &PageDataLayout, page: &Page, page_storage: &dyn PageStorage) -> Result<(), StoreError> {
        let data = page.serialize();

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(self.base_path.join(page_storage.file_path()))?;
        let page_pos = page.page_id() - 1;
        file.seek(SeekFrom::Start((layout.metadata_size() + page_pos as usize * layout.page_size()) as u64))?;
        file.write_all(&data)?;
        Ok(())
    }
    
    fn allocate_page<'database>(&self, layout: &'database PageDataLayout, page_storage: &dyn PageStorage) -> Result<Page<'database>, StoreError> {
        let mut metadata = self.read_metadata(layout, page_storage)?;
        let mut new_page = Page::new(layout);
        new_page.set_page_id(metadata.allocate_next_page_id());
        
        // ToDo: here we can get into an inconsistent state if write_page fails after write_metadata succeeded
        self.write_metadata(layout, &metadata, page_storage)?;
        self.write_page(layout, &new_page, page_storage)?;
        Ok(new_page)
    }
    
    fn create(&self, layout: &PageDataLayout, page_storage: &dyn PageStorage) -> Result<(), StoreError> {
        if std::fs::exists(self.file_path(page_storage))? {
            return Err(StoreError::IoError(format!("Data structure '{}' already exists", page_storage.file_path())));
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{data::page::PageDataLayout, store::{PageIterator, Store, file_store::FileStore}, table::{Column, ColumnType, TableSchema, table::{Cell, Row, Table}}};

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

            Self {
                col_id,
                current,
            }
        }
    }

    #[test]
    fn should_allocate_and_write_page() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path());

        let layout = PageDataLayout::new(128).unwrap();

        let schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int)
        ]);

        let table = Table::new(1, "test".to_owned(), schema);

        store.create(&layout, &table).unwrap();
        let mut new_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(new_page.page_id(), 1);

        let row = Row::new(vec![
            Cell::Int(42)
        ]);

        new_page.insert_record(row.serialize()).unwrap();

        store.write_page(&layout, &new_page, &table).unwrap();

        let loaded_page = store.read_page(&layout, 1, &table).unwrap();

        let row = Row::deserialize(loaded_page.row_data(), table.schema());

        assert_eq!(row.cells().len(), 1);
        matches!(row.cells().get(0).unwrap(), Cell::Int(42));
    }

    #[test]
    fn should_can_allocate_twice() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path());

        let layout = PageDataLayout::new(128).unwrap();

        let schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int)
        ]);

        let table = Table::new(1, "test".to_owned(), schema);

        store.create(&layout, &table).unwrap();
        // Create first page (stays empty)
        let first_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(first_page.page_id(), 1);

        store.write_page(&layout, &first_page, &table).unwrap();

        // Create second page with a row
        let mut second_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(second_page.page_id(), 2);

        let row = Row::new(vec![
            Cell::Int(42)
        ]);

        second_page.insert_record(row.serialize()).unwrap();
        store.write_page(&layout, &second_page, &table).unwrap();
        let loaded_page = store.read_page(&layout, 2, &table).unwrap();

        let row = Row::deserialize(loaded_page.row_data(), table.schema());

        assert_eq!(row.cells().len(), 1);
        matches!(row.cells().get(0).unwrap(), Cell::Int(42));
    }

    #[test]
    fn should_be_able_to_store_arbitrary_data() {
        let dir = tempdir().unwrap();
        let store = FileStore::new(dir.path());

        let layout = PageDataLayout::new(128).unwrap();

        let schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int)
        ]);
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
        let store = FileStore::new(dir.path());

        let layout = PageDataLayout::new(32).unwrap();

        let schema = TableSchema::new(vec![
            Column::new(1, "id", ColumnType::Int)
        ]);

        let table = Table::new(1, "test".to_owned(), schema);

        store.create(&layout, &table).unwrap();
        let mut new_page = store.allocate_page(&layout, &table).unwrap();
        assert_eq!(new_page.page_id(), 1);

        let row = Row::new(vec![
            Cell::Int(42)
        ]);

        new_page.insert_record(row.serialize()).unwrap();
        store.write_page(&layout, &new_page, &table).unwrap();

        let mut iter = PageIterator::new(&table, &store, &layout);

        let page = iter.next().unwrap();

        assert_eq!(page.page_id(), 1);
        matches!(page.data_offset(), 28);
    }
}

