use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::Path,
    rc::Rc,
    time::Duration,
};

use thiserror::Error;
use ttl_cache::TtlCache;

use crate::tree::node::NodeOperationError;

use super::{get_i32_be_bytes_from_option, node::NodePage, read_i32_with_null};

// File design:
// NULL values are currently designed with sentinel values: i32::MIN
// This is not efficient and is only used for simplicity.
// A better approach would be to use a bitmap to indicate which values are null and which are not.
// For example: for 16 possible values store 2 extra bytes to model null values in bits.

// Metadata header => 14 Bytes
// 2 bytes: max_degree
// 4 bytes: number_of_pages (max: u32:MAX - 1)
// 4 bytes: first_deleted_page (i32::MIN for INVALID / NULL)
// 4 bytes: root (i32::MIN for INVALID / NULL)
// -----------------------------------
// Page
// Meta-Section:
// 4 Bytes: page_id
const POS_PAGE_ID: usize = 0;
// 1 Byte: Deleted (0x00: not deleted, everything else: deleted)
const POS_DELETED: usize = 4;
// 4 bytes: next_deleted_page (number of next deleted page, i32::MIN for INVALID / NULL)
const POS_NEXT_DELETED_PAGE: usize = 5;
// Node-Section:
// 9 x 4 bytes: keys
// 10 x 4 bytes: pageIds
// 9 x 8 bytes: values (tuples of i32, i32)
const NEXT_LEAF_SIZE: usize = 4;
// 4 bytes: next_leaf (linked list) i32::MIN for NULL

// just for playing around, should be encoded in meta data header.
const PAGE_DATA_START: usize = 9; // same as PAGE_HEADER_SIZE
const PAGE_HEADER_SIZE: usize = 9;
const META_DATA_HEADER_SIZE: usize = 14;

fn key_offset() -> usize {
    PAGE_DATA_START
}

fn key_array_size(max_degree: u16) -> usize {
    ((max_degree - 1) * 4) as usize
}

fn children_offset(max_degree: u16) -> usize {
    key_offset() + key_array_size(max_degree)
}

fn children_array_size(max_degree: u16) -> usize {
    max_degree as usize * 4
}

fn values_offset(max_degree: u16) -> usize {
    children_offset(max_degree) + children_array_size(max_degree)
}

fn values_array_size(max_degree: u16) -> usize {
    ((max_degree - 1) * 8) as usize
}

fn next_leaf_offset(max_degree: u16) -> usize {
    // size of values is now 8 bytes per tuple (i32, i32)
    values_offset(max_degree) + values_array_size(max_degree)
}

fn meta_data_to_bytes(store_meta_data: &StoreMetaData) -> Vec<u8> {
    let mut metadata_bytes = [0u8; META_DATA_HEADER_SIZE];
    metadata_bytes[0..2].copy_from_slice(&store_meta_data.max_degree.to_be_bytes());
    metadata_bytes[2..6].copy_from_slice(&store_meta_data.number_of_pages.to_be_bytes());
    metadata_bytes[6..10].copy_from_slice(&get_i32_be_bytes_from_option(
        &store_meta_data.first_deleted_page,
    ));
    metadata_bytes[10..14].copy_from_slice(&get_i32_be_bytes_from_option(&store_meta_data.root));
    metadata_bytes.to_vec()
}

fn slice_to_array_4(bytes: &[u8], field: &str) -> Result<[u8; 4], NodePagerError> {
    bytes
        .try_into()
        .map_err(|_| NodePagerError::CorruptedPage(format!("Invalid {field} field length")))
}

fn slice_to_array_2(bytes: &[u8], field: &str) -> Result<[u8; 2], BTreeStoreError> {
    bytes
        .try_into()
        .map_err(|_| BTreeStoreError::CorruptedMetadata(format!("Invalid {field} field length")))
}

fn read_node_i32(page_bytes: &[u8], start: usize, field: &str) -> Result<i32, NodePagerError> {
    let bytes = page_bytes
        .get(start..start + 4)
        .ok_or_else(|| NodePagerError::CorruptedPage(format!("Missing {field} field")))?;
    Ok(i32::from_be_bytes(slice_to_array_4(bytes, field)?))
}

fn read_meta_i32(metadata_bytes: &[u8], start: usize, field: &str) -> Result<i32, BTreeStoreError> {
    let bytes = metadata_bytes
        .get(start..start + 4)
        .ok_or_else(|| BTreeStoreError::CorruptedMetadata(format!("Missing {field} field")))?;
    let arr: [u8; 4] = bytes
        .try_into()
        .map_err(|_| BTreeStoreError::CorruptedMetadata(format!("Invalid {field} field length")))?;
    Ok(i32::from_be_bytes(arr))
}

fn read_meta_u32(metadata_bytes: &[u8], start: usize, field: &str) -> Result<u32, BTreeStoreError> {
    let bytes = metadata_bytes
        .get(start..start + 4)
        .ok_or_else(|| BTreeStoreError::CorruptedMetadata(format!("Missing {field} field")))?;
    let arr: [u8; 4] = bytes
        .try_into()
        .map_err(|_| BTreeStoreError::CorruptedMetadata(format!("Invalid {field} field length")))?;
    Ok(u32::from_be_bytes(arr))
}

fn read_meta_u16(metadata_bytes: &[u8], start: usize, field: &str) -> Result<u16, BTreeStoreError> {
    let bytes = metadata_bytes
        .get(start..start + 2)
        .ok_or_else(|| BTreeStoreError::CorruptedMetadata(format!("Missing {field} field")))?;
    Ok(u16::from_be_bytes(slice_to_array_2(bytes, field)?))
}

#[derive(Debug)]
pub struct StoreMetaData {
    max_degree: u16,
    number_of_pages: u32, // in total: with deleted pages
    first_deleted_page: Option<i32>,
    root: Option<i32>,
    changed: bool, // will not be serialized, is only a flag, if NodePager has changed the meta data
    unique_index: bool, // not serialized yet, because I only deal with unique indexes at the moment (so it's always true)
}

impl StoreMetaData {
    pub fn inc_number_of_pages(&mut self) {
        self.number_of_pages += 1;
        self.changed = true;
    }

    pub fn set_first_deleted_page(&mut self, page: Option<i32>) {
        self.first_deleted_page = page;
        self.changed = true;
    }

    pub fn set_root(&mut self, root_page_id: i32) {
        self.root = Some(root_page_id);
        self.changed = true;
    }
}

impl TryFrom<(Vec<u8>, u16)> for NodePage {
    type Error = NodePagerError;

    fn try_from(page_bytes_and_degree: (Vec<u8>, u16)) -> Result<Self, Self::Error> {
        let page_bytes = page_bytes_and_degree.0;
        let max_degree = page_bytes_and_degree.1;

        let page_id = read_node_i32(&page_bytes, POS_PAGE_ID, "page_id")?;

        if page_id == i32::MIN {
            return Err(NodePagerError::InvalidNodePage(
                "Read a page with INVALID id".to_owned(),
            ));
        }

        let deleted = !matches!(page_bytes[POS_DELETED], 0);

        let next_deleted_page = read_i32_with_null(read_node_i32(
            &page_bytes,
            POS_NEXT_DELETED_PAGE,
            "next_deleted_page",
        )?);

        let mut keys = Vec::new();
        let key_offset = key_offset();
        for k in 0..(max_degree - 1) {
            let next_offset = key_offset + (k as usize * 4);
            let next_key = read_i32_with_null(read_node_i32(&page_bytes, next_offset, "key")?);
            if let Some(next_key) = next_key {
                keys.push(next_key);
            } else {
                break;
            }
        }

        let mut children = Vec::new();

        let child_offset = children_offset(max_degree);
        for c in 0..max_degree {
            let next_offset = child_offset + (c as usize * 4);
            let next_child = read_i32_with_null(read_node_i32(&page_bytes, next_offset, "child")?);
            if let Some(next_child) = next_child {
                children.push(next_child);
            } else {
                break;
            }
        }

        let mut values = Vec::new();

        let value_offset = values_offset(max_degree);
        for v in 0..(max_degree - 1) {
            let next_offset = value_offset + (v as usize * 8);
            let val1 = read_node_i32(&page_bytes, next_offset, "value.0")?;
            let val2 = read_node_i32(&page_bytes, next_offset + 4, "value.1")?;
            if val1 != i32::MIN && val2 != i32::MIN {
                values.push((val1, val2));
            } else {
                break;
            }
        }

        let next_leaf_offset = next_leaf_offset(max_degree);
        let next_leaf =
            read_i32_with_null(read_node_i32(&page_bytes, next_leaf_offset, "next_leaf")?);

        Ok(NodePage::new_from_store(
            page_id,
            deleted,
            next_deleted_page,
            keys,
            children,
            values,
            next_leaf,
            max_degree as usize,
        ))
    }
}

pub struct NodePager {
    file: RefCell<File>,
    cache: RefCell<TtlCache<i32, NodePage>>,
    meta_data: Rc<RefCell<StoreMetaData>>,
}

#[derive(Debug, Error)]
pub enum NodePagerError {
    #[error("NodePager I/O error")]
    Io(#[from] std::io::Error),
    #[error("Invalid node page: {0}")]
    InvalidNodePage(String),
    #[error("Corrupted node page: {0}")]
    CorruptedPage(String),
    #[error("Invalid page id: {0}")]
    InvalidPageId(String),
}

impl NodePager {
    fn new(file: File, meta_data: Rc<RefCell<StoreMetaData>>) -> Self {
        let cache = TtlCache::new(500);
        NodePager {
            file: RefCell::new(file),
            cache: RefCell::new(cache),
            meta_data,
        }
    }

    pub fn page_size(&self) -> u32 {
        let meta_data = self.meta_data.borrow();
        let children = (meta_data.max_degree * 4) as u32;
        let keys = ((meta_data.max_degree - 1) * 4) as u32;
        let values = ((meta_data.max_degree - 1) * 8) as u32; // tuples are 8 bytes each

        children + keys + values + NEXT_LEAF_SIZE as u32 + PAGE_HEADER_SIZE as u32
    }

    fn page_offset(&self, page_id: i32) -> Result<u64, NodePagerError> {
        if page_id < 0 {
            return Err(NodePagerError::InvalidPageId(format!("page_id={page_id}")));
        }

        Ok(META_DATA_HEADER_SIZE as u64 + (self.page_size() as u64 * page_id as u64))
    }

    pub fn write_page(&self, node: &NodePage) -> Result<(), NodePagerError> {
        if !*node.changed().borrow() {
            return Ok(());
        }
        if *node.id() == i32::MIN {
            return Err(NodePagerError::InvalidNodePage(
                "Cannot save page with the id i32::MIN".to_owned(),
            ));
        }
        if *node.deleted() {
            return Err(NodePagerError::InvalidNodePage(
                "Cannot write deleted pages. Use delete for this operation".to_owned(),
            ));
        }

        let meta_data = self.meta_data.borrow();
        let mut file = self.file.borrow_mut();
        let mut data = vec![0; self.page_size() as usize];

        // Prefill the whole node payload with NULL sentinel (i32::MIN).
        // Start at key_offset (9), so 4-byte writes stay aligned with all payload fields.
        for offset in (key_offset()..data.len()).step_by(4) {
            data[offset..(offset + 4)].copy_from_slice(&i32::MIN.to_be_bytes());
        }

        // build page header
        data[POS_PAGE_ID..POS_PAGE_ID + 4].copy_from_slice(&node.id().to_be_bytes());
        data[POS_DELETED] = match node.deleted() {
            true => 1,
            false => 0,
        };
        data[POS_NEXT_DELETED_PAGE..POS_NEXT_DELETED_PAGE + 4]
            .copy_from_slice(&get_i32_be_bytes_from_option(node.next_deleted_page()));

        // build Node
        // Serialize Keys:
        let key_offset = key_offset();

        for (i, k) in node.keys().iter().enumerate() {
            let current_offset = key_offset + (i * 4);
            data[current_offset..(current_offset + 4)].copy_from_slice(&k.to_be_bytes());
        }
        let child_offset = children_offset(meta_data.max_degree);

        // Serialize children:
        for (i, c) in node.children().iter().enumerate() {
            let current_offset = child_offset + (i * 4);
            data[current_offset..(current_offset + 4)].copy_from_slice(&c.to_be_bytes());
        }

        let values_offset = values_offset(meta_data.max_degree);

        // Serialize values
        for (i, v) in node.values().iter().enumerate() {
            let current_offset = values_offset + (i * 8);
            data[current_offset..(current_offset + 4)].copy_from_slice(&v.0.to_be_bytes());
            data[(current_offset + 4)..(current_offset + 8)].copy_from_slice(&v.1.to_be_bytes());
        }

        let next_leaf_offset = next_leaf_offset(meta_data.max_degree);

        // Serialize next_leaf (linked list between leafs)
        data[next_leaf_offset..(next_leaf_offset + 4)]
            .copy_from_slice(&get_i32_be_bytes_from_option(node.next_leaf()));

        let offset = self.page_offset(*node.id())?;
        file.seek(std::io::SeekFrom::Start(offset))?;
        file.write_all(&data)?;

        *node.changed().borrow_mut() = false;

        self.cache
            .borrow_mut()
            .insert(*node.id(), node.clone(), Duration::from_secs(120));

        Ok(())
    }

    pub fn read_page(&self, page_id: i32) -> Result<NodePage, NodePagerError> {
        if let Some(node) = self.cache.borrow().get(&page_id) {
            return Ok(node.clone());
        }

        let mut file = self.file.borrow_mut();
        let mut data = vec![0; self.page_size() as usize];
        let offset = self.page_offset(page_id)?;
        file.seek(std::io::SeekFrom::Start(offset))?;
        file.read_exact(&mut data)?;

        NodePage::try_from((data, self.meta_data.borrow().max_degree))
    }

    pub fn delete_page(&self, page_id: i32) -> Result<(), NodePagerError> {
        if page_id == i32::MIN {
            return Err(NodePagerError::InvalidPageId(
                "Cannot delete page_id i32::MIN".to_owned(),
            ));
        }

        let first_deleted_page = self.meta_data.borrow().first_deleted_page;
        let mut node = self.read_page(page_id)?;
        node.delete_page(first_deleted_page);
        self.meta_data
            .borrow_mut()
            .set_first_deleted_page(Some(*node.id()));

        Ok(())
    }

    pub fn allocate_new_page(&self) -> Result<NodePage, NodePagerError> {
        // Is there a deleted page?
        let first_deleted = self.meta_data.borrow().first_deleted_page;
        if let Some(first_deleted) = first_deleted {
            match self.read_page(first_deleted) {
                Ok(mut allocated) => {
                    self.meta_data
                        .borrow_mut()
                        .set_first_deleted_page(*allocated.next_deleted_page());
                    allocated.reallocate();
                    self.write_page(&allocated)?;
                    Ok(allocated)
                }
                Err(e) => Err(NodePagerError::CorruptedPage(format!(
                    "Failed to reallocate page with ID = {first_deleted}, err = {e}"
                ))),
            }
        } else {
            self.meta_data.borrow_mut().inc_number_of_pages();
            // first id is 0;
            let next_id = self.meta_data.borrow().number_of_pages - 1;
            let next_id = i32::try_from(next_id).map_err(|_| {
                NodePagerError::InvalidPageId(
                    "Cannot allocate page id greater than i32::MAX".to_owned(),
                )
            })?;
            let node = NodePage::new(self.meta_data.borrow().max_degree as usize, next_id);
            self.write_page(&node)?;
            // is likely to change after allocation
            *node.changed().borrow_mut() = true;
            Ok(node)
        }
    }
}

pub struct BTreeStore {
    pager: NodePager,
    meta_data: Rc<RefCell<StoreMetaData>>,
}

#[derive(Debug, Error)]
pub enum BTreeStoreError {
    #[error("B+ Tree invalid configuration: {0}")]
    InvalidConfiguration(String),
    #[error("B+ Tree metadata is corrupted: {0}")]
    CorruptedMetadata(String),
    #[error("B+ Tree pager error: {0}")]
    NodePager(#[from] NodePagerError),
    #[error("B+ Tree node operation error: {0}")]
    NodeOperation(#[from] NodeOperationError),
}

impl BTreeStore {
    pub fn new(file_path: &Path, max_degree: u16) -> Result<Self, BTreeStoreError> {
        if max_degree < 4 {
            return Err(BTreeStoreError::InvalidConfiguration(
                "BTreeStore must have at least a max degree of 4".to_owned(),
            ));
        }

        if max_degree % 2 != 0 {
            return Err(BTreeStoreError::InvalidConfiguration(
                "BTreeStore must have an even number for max degree".to_owned(),
            ));
        }

        let store_meta_data;

        let file_size = fs::metadata(file_path)
            .ok()
            .map(|fmd| fmd.len())
            .unwrap_or(0);

        let file = match OpenOptions::new().read(true).write(true).open(file_path) {
            Ok(mut f) if file_size >= META_DATA_HEADER_SIZE as u64 => {
                let mut metadata_bytes = [0u8; META_DATA_HEADER_SIZE];
                f.read_exact(&mut metadata_bytes)
                    .map_err(NodePagerError::from)
                    .map_err(BTreeStoreError::from)?;

                let max_degree = read_meta_u16(&metadata_bytes, 0, "max_degree")?;
                let number_of_pages = read_meta_u32(&metadata_bytes, 2, "number_of_pages")?;
                let first_deleted_page = read_meta_i32(&metadata_bytes, 6, "first_deleted_page")?;
                let root = read_meta_i32(&metadata_bytes, 10, "root")?;

                store_meta_data = StoreMetaData {
                    max_degree,
                    number_of_pages,
                    first_deleted_page: read_i32_with_null(first_deleted_page),
                    root: read_i32_with_null(root),
                    changed: false,
                    unique_index: true,
                };

                f
            }
            _ => {
                let mut f = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(file_path)
                    .map_err(NodePagerError::from)
                    .map_err(BTreeStoreError::from)?;

                store_meta_data = StoreMetaData {
                    max_degree,
                    number_of_pages: 0,
                    first_deleted_page: None,
                    root: None,
                    changed: false,
                    unique_index: true,
                };

                let metadata_bytes = meta_data_to_bytes(&store_meta_data);

                f.write_all(&metadata_bytes)
                    .map_err(NodePagerError::from)
                    .map_err(BTreeStoreError::from)?;
                f.flush()
                    .map_err(NodePagerError::from)
                    .map_err(BTreeStoreError::from)?;

                f
            }
        };

        let rc_meta_data = Rc::new(RefCell::new(store_meta_data));

        Ok(BTreeStore {
            pager: NodePager::new(file, Rc::clone(&rc_meta_data)),
            meta_data: rc_meta_data,
        })
    }

    #[allow(dead_code)]
    fn page_size(&self) -> u32 {
        self.pager.page_size()
    }

    #[cfg(test)]
    pub fn print_nodes(&self) {
        match self.root() {
            Ok(r) => r.print_all_nodes(&self.pager),
            _ => (),
        }
    }

    #[cfg(test)]
    pub fn validate(&self) {
        self.root().unwrap().validate(&self.pager, None, None);
    }

    pub fn next_node(&self, node: &NodePage) -> Result<Option<NodePage>, BTreeStoreError> {
        if let Some(page_id) = node.next_leaf() {
            Ok(Some(self.pager.read_page(*page_id)?))
        } else {
            Ok(None)
        }
    }

    pub fn find_smaller_than(
        &self,
        key: i32,
        with_equal: bool,
    ) -> Result<Vec<(i32, i32)>, BTreeStoreError> {
        let mut result = Vec::new();
        let mut node = self.find_left_most_node()?;
        let target_node = self.find_node(key)?;

        if let Some(target_node) = target_node {
            while let Some(next) = node {
                if *target_node.id() == *next.id() {
                    for i in 0..next.keys().len() {
                        if next.keys()[i] < key || (with_equal && next.keys()[i] == key) {
                            result.push(next.values()[i]);
                        }
                    }
                    break;
                } else {
                    result.extend(next.values());
                }

                node = self.next_node(&next)?
            }
        }

        Ok(result)
    }

    pub fn find_greater_than(
        &self,
        key: i32,
        with_equal: bool,
    ) -> Result<Vec<(i32, i32)>, BTreeStoreError> {
        let mut result = Vec::new();
        let mut node = self.find_node(key)?;

        let mut start_node = true;
        while let Some(next) = node {
            if start_node {
                // The first node might contain multiple keys that are smaller than the key being searched for
                for i in 0..next.keys().len() {
                    if next.keys()[i] > key || (with_equal && next.keys()[i] == key) {
                        result.push(next.values()[i]);
                    }
                }
                start_node = false;
            } else {
                result.extend_from_slice(next.values());
            }

            node = self.next_node(&next)?
        }

        Ok(result)
    }

    pub fn find_left_most_node(&self) -> Result<Option<NodePage>, BTreeStoreError> {
        let root = self.root()?;

        if let Some(page_id) = root.find_left_most_node(&self.pager)? {
            Ok(Some(self.pager.read_page(page_id)?))
        } else {
            Ok(None)
        }
    }

    pub fn find_node(&self, key: i32) -> Result<Option<NodePage>, BTreeStoreError> {
        let root = self.root()?;

        if let Some(page_id) = root.find_node(&self.pager, key)? {
            Ok(Some(self.pager.read_page(page_id)?))
        } else {
            Ok(None)
        }
    }

    pub fn find(&self, key: i32) -> Result<Option<(i32, i32)>, BTreeStoreError> {
        let root = self.root()?;

        Ok(root.find_value(&self.pager, key)?)
    }

    pub fn insert(&mut self, key: i32, value: (i32, i32)) -> Result<(), BTreeStoreError> {
        let mut root = self.root()?;
        if root.is_full() {
            let (rnode, root_key) = root.split(&self.pager)?;
            let mut new_root = self
                .pager
                .allocate_new_page()
                .map_err(BTreeStoreError::from)?;
            new_root.keys_mut().push(root_key);
            // root becomes the left node
            new_root.children_mut().push(*root.id());
            new_root.children_mut().push(*rnode.id());

            self.meta_data.borrow_mut().root = Some(*new_root.id());

            // for the insert: set variable root to new_root
            root = new_root;
            *root.changed().borrow_mut() = true;
        }

        let unique = self.meta_data.borrow().unique_index;
        root.insert(&self.pager, key, value, unique)?;
        self.pager
            .write_page(&root)
            .map_err(BTreeStoreError::from)?;

        self.save_metadata()?;

        Ok(())
    }

    pub fn delete(&mut self, key: i32) -> Result<Option<(i32, i32)>, BTreeStoreError> {
        let mut root = self.root()?;
        let res = root.delete(&self.pager, key)?;

        if root.keys().is_empty() && !root.is_leaf() {
            // Special case where keys are empty and children has length 1 (after merging)
            // This case exists because of the preemptive merge on delete:
            // The root has exactly one key and the two children will be merged
            // The separator key is then taken from the root, so the the root has no key left after this action.
            // The only child gets the new root.
            debug_assert_eq!(
                root.children().len(),
                1,
                "Internal root node must have exactly 1 child when it is out of keys"
            );
            let new_root = root.children_mut().remove(0);
            self.pager.delete_page(*root.id())?;
            root = self.pager.read_page(new_root)?;
            self.meta_data.borrow_mut().root = Some(new_root);
        }

        self.pager.write_page(&root)?;
        self.save_metadata()?;

        Ok(res)
    }

    fn save_metadata(&self) -> Result<(), BTreeStoreError> {
        let changed = self.meta_data.borrow().changed;

        if changed {
            let bytes = meta_data_to_bytes(&self.meta_data.borrow());
            let mut file = self.pager.file.borrow_mut();

            file.seek(std::io::SeekFrom::Start(0))
                .map_err(NodePagerError::from)
                .map_err(BTreeStoreError::from)?;
            file.write_all(&bytes)
                .map_err(NodePagerError::from)
                .map_err(BTreeStoreError::from)?;
        }

        Ok(())
    }

    pub fn root(&self) -> Result<NodePage, BTreeStoreError> {
        let root = self.meta_data.borrow().root;

        match root {
            Some(root_id) => Ok(self.pager.read_page(root_id)?),
            None => {
                let mut new_root = self.pager.allocate_new_page()?;
                self.meta_data.borrow_mut().root = Some(*new_root.id());
                self.save_metadata()?;
                Ok(new_root)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;

    use tempfile::NamedTempFile;

    use crate::tree::{node::NodePage, store::BTreeStore};

    #[test]
    fn should_be_valid_after_lot_of_inserts_and_deletes() {
        let temp = NamedTempFile::new().unwrap();
        let mut tree = BTreeStore::new(temp.path(), 8).unwrap();
        let max_inserts = 1000;

        #[derive(Debug)]
        struct CurrentState {
            inserted_even: i32,
            inserted_uneven: i32,
            deleted: i32,
            do_validate: bool,
        }

        let mut current_state = CurrentState {
            inserted_even: 0,
            inserted_uneven: 0,
            deleted: 0,
            do_validate: false,
        };

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            // Insert even numbers
            for i in (2..max_inserts).step_by(2) {
                tree.insert(i, (0, 0));
                current_state.inserted_even += 1;
            }

            // Insert odd numbers
            for i in (1..max_inserts).step_by(2) {
                tree.insert(i, (0, 0));
                current_state.inserted_uneven += 1;
            }

            // Delete every 3rd element
            for i in (0..max_inserts).step_by(3) {
                tree.delete(i);
                current_state.deleted += 1;
            }

            current_state.do_validate = true;
            tree.validate();
        }));

        assert!(
            result.is_ok(),
            "B+ Tree panicked at: {:?} (max_inserts: {})",
            current_state,
            max_inserts
        );
    }

    #[test]
    fn find_left_most_node() {
        let temp = NamedTempFile::new().unwrap();
        let mut btree = BTreeStore::new(temp.path(), 4).unwrap();
        btree.insert(1, (1, 1)).unwrap();
        btree.insert(10, (10, 10)).unwrap();
        btree.insert(2, (2, 2)).unwrap();
        btree.insert(5, (5, 5)).unwrap();
        btree.insert(100, (100, 100)).unwrap();
        btree.insert(3, (3, 3)).unwrap();
        btree.insert(4, (4, 4)).unwrap();
        btree.insert(50, (50, 50)).unwrap();
        btree.insert(20, (20, 20)).unwrap();

        let mut node = btree.find_left_most_node().unwrap().unwrap();
        assert_eq!(node.keys(), &vec![1i32]);
    }

    #[test]
    fn find_all_smaller_than() {
        let temp = NamedTempFile::new().unwrap();
        let mut btree = BTreeStore::new(temp.path(), 4).unwrap();
        btree.insert(1, (1, 1)).unwrap();
        btree.insert(10, (10, 10)).unwrap();
        btree.insert(2, (2, 2)).unwrap();
        btree.insert(5, (5, 5)).unwrap();
        btree.insert(100, (100, 100)).unwrap();
        btree.insert(3, (3, 3)).unwrap();
        btree.insert(4, (4, 4)).unwrap();
        btree.insert(50, (50, 50)).unwrap();
        btree.insert(20, (20, 20)).unwrap();
        btree.insert(6, (6, 6)).unwrap();
        btree.insert(7, (7, 7)).unwrap();
        btree.insert(8, (8, 8)).unwrap();

        let mut result = btree.find_smaller_than(20, true).unwrap();
        assert_eq!(
            result,
            vec![
                (1, 1),
                (2, 2),
                (3, 3),
                (4, 4),
                (5, 5),
                (6, 6),
                (7, 7),
                (8, 8),
                (10, 10),
                (20, 20)
            ]
        );

        let mut result = btree.find_smaller_than(20, false).unwrap();
        assert_eq!(
            result,
            vec![
                (1, 1),
                (2, 2),
                (3, 3),
                (4, 4),
                (5, 5),
                (6, 6),
                (7, 7),
                (8, 8),
                (10, 10)
            ]
        );
    }

    #[test]
    fn find_all_greater_than() {
        let temp = NamedTempFile::new().unwrap();
        let mut btree = BTreeStore::new(temp.path(), 4).unwrap();
        btree.insert(1, (1, 1)).unwrap();
        btree.insert(10, (10, 10)).unwrap();
        btree.insert(2, (2, 2)).unwrap();
        btree.insert(5, (5, 5)).unwrap();
        btree.insert(100, (100, 100)).unwrap();
        btree.insert(3, (3, 3)).unwrap();
        btree.insert(4, (4, 4)).unwrap();
        btree.insert(50, (50, 50)).unwrap();
        btree.insert(20, (20, 20)).unwrap();
        btree.insert(6, (6, 6)).unwrap();
        btree.insert(7, (7, 7)).unwrap();
        btree.insert(8, (8, 8)).unwrap();

        let mut result = btree.find_greater_than(7, true).unwrap();
        assert_eq!(
            result,
            vec![(7, 7), (8, 8), (10, 10), (20, 20), (50, 50), (100, 100)]
        );

        let mut result = btree.find_greater_than(7, false).unwrap();
        assert_eq!(
            result,
            vec![(8, 8), (10, 10), (20, 20), (50, 50), (100, 100)]
        );
    }

    #[test]
    fn delete_everything_except_one_key() {
        let temp = NamedTempFile::new().unwrap();
        let mut btree = BTreeStore::new(temp.path(), 4).unwrap();
        btree.insert(1, (1, 1)).unwrap();
        btree.insert(10, (10, 10)).unwrap();
        btree.insert(2, (2, 2)).unwrap();
        btree.insert(5, (5, 5)).unwrap();
        btree.insert(100, (100, 100)).unwrap();

        btree.delete(1).unwrap();
        btree.delete(10).unwrap();
        btree.delete(2).unwrap();
        btree.delete(5).unwrap();

        let row_page = btree.find(100).unwrap();

        assert!(row_page.is_some());
        assert_eq!(row_page.unwrap(), (100, 100));

        let row_page = btree.find(5).unwrap();
        assert!(row_page.is_none());

        let row_page = btree.find(2).unwrap();
        assert!(row_page.is_none());
    }

    #[test]
    fn insert_delete_find() {
        let temp = NamedTempFile::new().unwrap();
        let mut btree = BTreeStore::new(temp.path(), 4).unwrap();
        btree.insert(1, (1, 1)).unwrap();
        btree.insert(10, (10, 10)).unwrap();
        btree.insert(2, (2, 2)).unwrap();
        btree.insert(5, (5, 5)).unwrap();
        btree.insert(100, (100, 100)).unwrap();

        let row_page = btree.find(2).unwrap();

        assert!(row_page.is_some());
        assert_eq!(row_page.unwrap(), (2, 2));

        // delete key=2 => merge happens: lend key 5 from right node before delete
        let deleted_value = btree.delete(2).unwrap();
        assert!(deleted_value.is_some());
        assert_eq!(deleted_value.unwrap(), (2, 2));

        // try to delete key=2 again will merge the middle with the right node again
        let deleted_value = btree.delete(2).unwrap();
        assert!(deleted_value.is_none());
        // should have lend two times from right node [5, 10, 100], so [5, 10] is on the middle and [100] is on the right, key parent should be 100.
        // Try to find the lend key 5 in middle node:
        let row_page = btree.find(5).unwrap();

        assert!(row_page.is_some());
        assert_eq!(row_page.unwrap(), (5, 5));

        // Try to find a value in the right most node after parents key has been updated:
        let row_page = btree.find(100).unwrap();

        assert!(row_page.is_some());
        assert_eq!(row_page.unwrap(), (100, 100));
    }

    #[test]
    fn insert_and_find() {
        let temp = NamedTempFile::new().unwrap();
        let mut btree = BTreeStore::new(temp.path(), 4).unwrap();
        btree.insert(1, (1, 1)).unwrap();
        btree.insert(10, (10, 10)).unwrap();
        btree.insert(2, (2, 2)).unwrap();
        btree.insert(5, (5, 5)).unwrap();
        btree.insert(100, (100, 100)).unwrap();
        btree.insert(3, (3, 3)).unwrap();
        btree.insert(4, (4, 4)).unwrap();

        let row_page = btree.find(100).unwrap();

        assert!(row_page.is_some());
        assert_eq!(row_page.unwrap(), (100, 100));
    }

    #[test]
    fn insert_and_find_in_root_only() {
        let temp = NamedTempFile::new().unwrap();
        let mut btree = BTreeStore::new(temp.path(), 4).unwrap();
        btree.insert(1, (1, 1)).unwrap();
        btree.insert(10, (10, 10)).unwrap();

        let row_page = btree.find(1).unwrap();

        assert!(row_page.is_some());
        assert_eq!(row_page.unwrap(), (1, 1));
    }

    #[test]
    fn get_root() {
        let temp = NamedTempFile::new().unwrap();
        let btree = BTreeStore::new(temp.path(), 10).unwrap();
        let root_res = btree.root();
        assert!(root_res.is_ok());
        let root = root_res.unwrap();
        assert_eq!(*root.id(), 0);
        assert_eq!(*root.deleted(), false);
        assert!(root.keys().is_empty());
        assert!(root.children().is_empty());
        assert!(root.values().is_empty());
    }

    #[test]
    fn reallocate_deleted_page() {
        // Arrange
        let temp = NamedTempFile::new().unwrap();
        let btree = BTreeStore::new(temp.path(), 10).unwrap();
        // allocate page1
        let mut page1 = btree.pager.allocate_new_page().unwrap();
        page1.keys_mut().push(1);
        page1.keys_mut().push(5);
        btree.pager.write_page(&page1).unwrap();

        // allocate page2
        let mut page2 = btree.pager.allocate_new_page().unwrap();
        page2.keys_mut().push(7);
        page2.keys_mut().push(10);
        btree.pager.write_page(&page2).unwrap();

        // delete page2
        btree.pager.delete_page(*page2.id()).unwrap();
        btree.save_metadata().unwrap();

        // Act
        // should now allocate the delete page page2 (id=1)
        let allocated1 = btree.pager.allocate_new_page().unwrap();
        // should allocate a new page
        let allocated2 = btree.pager.allocate_new_page().unwrap();

        // Assert
        assert_eq!(*allocated1.id(), 1);
        assert_eq!(*allocated1.deleted(), false);
        assert_eq!(*allocated1.next_deleted_page(), None);
        assert!(allocated1.keys().is_empty());
        assert!(allocated1.values().is_empty());

        assert_eq!(*allocated2.id(), 2);
        assert_eq!(*allocated2.deleted(), false);
        assert_eq!(*allocated2.next_deleted_page(), None);
    }

    #[test]
    fn write_and_read_pages() {
        let page1 = NodePage::new_from_store(
            0,
            false,
            None,
            vec![1, 5, 6],
            vec![3, 9, 10, 16],
            Vec::new(),
            None,
            4,
        );

        *page1.changed().borrow_mut() = true;

        let temp = NamedTempFile::new().unwrap();
        let btree = BTreeStore::new(temp.path(), 10).unwrap();
        btree.pager.write_page(&page1).unwrap();

        let page1_loaded = btree.pager.read_page(0).unwrap();

        assert_eq!(*page1_loaded.id(), 0);
        assert_eq!(*page1_loaded.deleted(), false);
        assert_eq!(*page1_loaded.next_deleted_page(), None);
        assert_eq!(*page1_loaded.keys(), vec![1, 5, 6]);
        assert_eq!(*page1_loaded.children(), vec![3, 9, 10, 16]);
        assert!(page1_loaded.values().is_empty());

        // page 2:
        let page2 = NodePage::new_from_store(
            1,
            false,
            None,
            vec![7, 8],
            Vec::new(),
            vec![(1, 2), (3, 4)],
            None,
            4,
        );
        *page2.changed().borrow_mut() = true;
        btree.pager.write_page(&page2).unwrap();
        let page2_loaded = btree.pager.read_page(1).unwrap();

        assert_eq!(*page2_loaded.id(), 1);
        assert_eq!(*page2_loaded.deleted(), false);
        assert_eq!(*page2_loaded.next_deleted_page(), None);
        assert_eq!(*page2_loaded.keys(), vec![7, 8]);
        assert_eq!(*page2_loaded.values(), vec![(1, 2), (3, 4)]);
    }

    #[test]
    fn max_degree_should_be_at_least_4() {
        let temp = NamedTempFile::new().unwrap();
        let btree = BTreeStore::new(temp.path(), 0);
        assert!(btree.is_err());
        let btree = BTreeStore::new(temp.path(), 3);
        assert!(btree.is_err());
        let btree = BTreeStore::new(temp.path(), 4);
        assert!(btree.is_ok());
        assert_eq!(btree.unwrap().page_size(), 65) // 9 + 4*4 + 3*4 + 3*8 + 4 = 65
    }

    #[test]
    fn create_new_btree_store() {
        let temp = NamedTempFile::new().unwrap();
        let btree = BTreeStore::new(temp.path(), 10).unwrap();
        let meta_data = btree.meta_data.borrow();
        assert_eq!(meta_data.first_deleted_page, None);
        assert_eq!(meta_data.max_degree, 10);
        assert_eq!(meta_data.number_of_pages, 0);

        // Open existing BTree with some random degree
        let btree = BTreeStore::new(temp.path(), 100).unwrap();
        let meta_data = btree.meta_data.borrow();
        assert_eq!(meta_data.first_deleted_page, None);
        assert_eq!(meta_data.max_degree, 10); // Use the degree from meta data section
        assert_eq!(meta_data.number_of_pages, 0);
        assert_eq!(btree.page_size(), 161) // 9 + 10*4 + 9*4 + 9*8 + 4 = 161
    }
}
