use std::{array::TryFromSliceError, rc::Rc};

use thiserror::Error;

#[derive(Debug)]
pub struct PageDataLayout {
    page_size: u16,
}

#[derive(Error, Debug)]
pub enum PageDataLayoutError {
    #[error("Invalid page size specified. Page size must be at least 32 bytes.")]
    InvalidPageSize,
}

impl PageDataLayout {
    const INDEX_NUMBER_ROWS: usize = 0;
    const INDEX_ROW_OFFSET: usize = 2;
    const INDEX_PAGE_ID: usize = 6;
    const INDEX_FREE_SLOTS_OFFSET: usize = 10;
    const INDEX_FREE_SLOTS_START: usize = 14;

    // table meta data: 4 bytes next_id, 4 bytes number_of_pages
    pub const META_DATA_SIZE: usize = 8;
    // page header: 2 bytes num_rows, 4 bytes data_offset, 4 bytes page_id, 4 bytes slots_offset
    const PAGE_HEADER_SIZE: u16 = 14;
    const MIN_PAGE_SIZE: u16 = 32; // just arbitrarily value so it's easy to test with few bytes

    // free data tuple constants
    pub const SLOT_SIZE: usize = 7; // 4 bytes offset, 2 bytes length, 1 byte deleted flag
    const SLOT_DELETED_INDEX: usize = 0;
    const SLOT_PAGE_OFFSET_INDEX: usize = 1;
    const SLOT_RECORD_LENGTH_INDEX: usize = 5;

    pub fn max_tupel_size(&self) -> usize {
        // This is, when the page is completely empty, so no slots are allocated and no free space is fragmented
        self.page_data_size() - PageDataLayout::SLOT_SIZE
    }

    pub fn new(page_size: u16) -> Result<Self, PageDataLayoutError> {
        if page_size < Self::MIN_PAGE_SIZE {
            return Err(PageDataLayoutError::InvalidPageSize);
        }

        if !page_size.is_power_of_two() {
            // The Binary tree needs this condition for example to calculate the the number of non-leaf-nodes
            return Err(PageDataLayoutError::InvalidPageSize);
        }

        Ok(Self { page_size })
    }

    pub fn page_size(&self) -> usize {
        self.page_size as usize
    }

    pub fn page_data_size(&self) -> usize {
        (self.page_size - Self::PAGE_HEADER_SIZE) as usize
    }

    pub fn metadata_size(&self) -> usize {
        Self::META_DATA_SIZE
    }
}

#[derive(Debug, Error)]
pub enum ReadMetadataError {
    #[error("Cannot deserialize PageFileMetadata: {0}")]
    DeserializationError(String),
}

impl From<TryFromSliceError> for ReadMetadataError {
    fn from(err: TryFromSliceError) -> Self {
        ReadMetadataError::DeserializationError(err.to_string())
    }
}

// TODO: This is really confusing
// PageFileMetadata is actually the meta data of the complete heap file
// should maybe not live here. Is more an implementation detail of Store and how the store handles page IDs
#[derive(Debug)]
pub struct PageFileMetadata {
    next_id: i32,         // There is currently just a signed int for ids
    number_of_pages: i32, // because of next_id being i32
}

impl PageFileMetadata {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            number_of_pages: 0,
        }
    }
    pub fn deserialize(buf: &[u8]) -> Result<Self, ReadMetadataError> {
        let next_id = i32::from_be_bytes(buf[0..4].try_into()?);
        let number_of_pages = i32::from_be_bytes(buf[4..8].try_into()?);
        Ok(Self {
            next_id,
            number_of_pages,
        })
    }
    pub fn serialize(&self, layout: &PageDataLayout) -> Vec<u8> {
        let mut buf = vec![0u8; layout.metadata_size()];
        buf[0..4].copy_from_slice(&self.next_id.to_be_bytes());
        buf[4..8].copy_from_slice(&self.number_of_pages.to_be_bytes());
        buf
    }
    pub fn next_id(&self) -> i32 {
        self.next_id
    }

    pub fn number_of_pages(&self) -> i32 {
        self.number_of_pages
    }

    // would be better to handle this completely in Store (not used anywhere in Page)
    // then it can be private
    pub fn allocate_next_page_id(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.number_of_pages += 1;
        id
    }
}

#[derive(Debug)]
struct Slot {
    record_length: u16,
    page_offset: usize, // stored as u32
    deleted: bool,
}

#[derive(Debug)]
pub struct RecordData {
    // I need ownership because of the lazy filter chain in QueryResult
    // Maybe a converter that directly converts a record to any type could bypass this step
    // so essentially: RecordIterator<RecordConverter>
    data: Rc<Vec<u8>>,
    slice: (usize, usize),
}

impl RecordData {
    pub fn new(data: Rc<Vec<u8>>, slice_from: usize, slice_to: usize) -> Self {
        Self {
            data,
            slice: (slice_from, slice_to),
        }
    }
    pub fn data(&self) -> &[u8] {
        &self.data[self.slice.0..self.slice.1]
    }
}

#[derive(Debug)]
pub struct Record {
    page_id: i32,
    record_index: usize,
    data: RecordData,
}

impl Record {
    pub fn data(&self) -> &[u8] {
        self.data.data()
    }

    pub fn page_id(&self) -> &i32 {
        &self.page_id
    }

    pub fn record_index(&self) -> &usize {
        &self.record_index
    }
}

pub struct RecordIterator {
    page_id: i32,
    data: Rc<Vec<u8>>,
    slots: Vec<Slot>,
    next_slot: usize,
}

impl RecordIterator {
    pub fn from_slots(mut page: Page, slot_ids: Vec<usize>) -> Self {
        let slots = page
            .slots
            .drain(..)
            .enumerate()
            .filter(|(index, _)| slot_ids.contains(index))
            .map(|(_, s)| s)
            .collect();

        Self {
            page_id: page.page_id(),
            data: Rc::new(page.data),
            slots,
            next_slot: 0,
        }
    }

    pub fn new(page: Page) -> Self {
        Self {
            page_id: page.page_id(),
            data: Rc::new(page.data),
            slots: page.slots,
            next_slot: 0,
        }
    }
}

fn record_from_slot(page_id: i32, data: Rc<Vec<u8>>, slot_index: usize, slot: &Slot) -> Record {
    let data_to = slot.page_offset + slot.record_length as usize;
    Record {
        page_id,
        record_index: slot_index,
        data: RecordData::new(data, slot.page_offset, data_to),
    }
}

impl Iterator for RecordIterator {
    type Item = Record;

    fn next(&mut self) -> Option<Self::Item> {
        let mut found = None;

        while self.next_slot < self.slots.len() && found.is_none() {
            let slot = &self.slots[self.next_slot];

            if !slot.deleted {
                let record =
                    record_from_slot(self.page_id, Rc::clone(&self.data), self.next_slot, slot);

                found = Some(record);
            }

            self.next_slot += 1;
        }

        found
    }
}

// Page Layout
// ------------
// Header
// ------------
// free_slots (go downwards)
// slot1
// slot2
// ...
// ...
// row2
// row1
// rows (go upwards)
#[derive(Debug)]
pub struct Page<'database> {
    pub data: Vec<u8>,
    slots: Vec<Slot>,
    layout: &'database PageDataLayout,
    // header
    number_of_records: u16,
    // data_offset is actually the free space pointer
    // offset uses usize internally for easier handling (but it's actually an i32)
    // starts from page_data_size and is heading towards 0
    data_offset: usize,
    // Place where the next free slot can be inserted:
    // stored as i32 (starts from 0)
    pub slots_offset: usize,
    page_id: i32, // it's because of id being a i32
}

#[derive(Error, Debug)]
pub enum PageError {
    #[error("Failed to insert row into page. Page is full.")]
    InsertRecord,
    #[error("Failed to update record")]
    UpdateRecord,
    #[error("Failed to deserialize page: {0}")]
    Deserialization(String),
}

impl From<TryFromSliceError> for PageError {
    fn from(value: TryFromSliceError) -> Self {
        PageError::Deserialization(value.to_string())
    }
}

#[cfg(target_pointer_width = "64")] // so that I can use always 8 bytes for usize
impl<'database> Page<'database> {
    pub fn new(layout: &'database PageDataLayout) -> Self {
        Self {
            layout,
            data: vec![0; layout.page_data_size()],
            data_offset: layout.page_data_size(),
            number_of_records: 0,
            page_id: 0,
            slots: Vec::new(),
            slots_offset: 0,
        }
    }

    pub fn record_iterator(self) -> RecordIterator {
        RecordIterator::new(self)
    }

    /// Returns all the data on the page even dead tuples
    pub fn row_data(&self) -> &[u8] {
        &self.data[self.data_offset..self.layout.page_data_size()]
    }
    pub fn data_offset(&self) -> usize {
        self.data_offset
    }

    pub fn row_data_size(&self) -> usize {
        self.layout.page_data_size() - self.data_offset
    }

    pub fn slot_size(&self) -> usize {
        self.slots.len() * PageDataLayout::SLOT_SIZE
    }

    pub fn num_rows(&self) -> u16 {
        self.number_of_records
    }

    pub fn page_id(&self) -> i32 {
        self.page_id
    }

    pub fn set_page_id(&mut self, page_id: i32) {
        self.page_id = page_id;
    }

    pub fn read_slot(&self, slot_id: usize) -> Option<&[u8]> {
        self.slots
            .get(slot_id)
            .filter(|slot| !slot.deleted)
            .map(|slot| self.read_data(slot))
    }

    fn max_fragmented_free_space(&self) -> usize {
        self.slots
            .iter()
            .filter(|s| s.deleted)
            .map(|s| s.record_length)
            .max()
            .unwrap_or(0) as usize
    }

    pub fn max_available_space(&self) -> usize {
        // page_data_size - row_data_size - slots in use
        let allocated_space =
            self.layout.page_data_size() - self.row_data_size() - self.slot_size();
        std::cmp::max(allocated_space, self.max_fragmented_free_space())
    }

    pub fn can_insert(&self, row_bytes: &[u8]) -> bool {
        // currently can_insert is called twice
        // one time by the caller to check if the data fits in the page
        // and a second time by the page itself to verify that it really fits (maybe the caller didn't check)
        // the first call can be replaced by using FSM in the future
        if let Some(slot) = self.find_free_slot_index(row_bytes) {
            let perfect_fit = self
                .slots
                .get(slot)
                .map(|s| s.record_length as usize == row_bytes.len())
                .unwrap_or(false);

            if perfect_fit {
                return true;
            }
        }

        let needed_space = row_bytes.len() + PageDataLayout::SLOT_SIZE;
        needed_space <= self.max_available_space()
    }

    // just returns the index, so that the caller can decide if it wants to get the slot mutable or not.
    fn find_free_slot_index(&'database self, row_bytes: &[u8]) -> Option<usize> {
        let mut fallback = None;

        for (index, s) in self.slots.iter().enumerate() {
            if !s.deleted {
                continue;
            }

            if s.record_length == row_bytes.len() as u16 {
                // try to find a perfect fit
                fallback = Some(index);
                break;
            }

            if s.record_length >= row_bytes.len() as u16 && fallback.is_none() {
                fallback = Some(index);
            }
        }

        fallback
    }

    /// Update in place
    /// Only possible if the length hasn't changed
    pub fn write_record(
        &mut self,
        record_index: usize,
        row_bytes: Vec<u8>,
    ) -> Result<(), PageError> {
        let slot = self.slots.get_mut(record_index);

        if let Some(slot) = slot {
            if slot.record_length != row_bytes.len() as u16 {
                // Cannot update in place if length differ
                return Err(PageError::UpdateRecord);
            }

            let end_of_data = slot.page_offset + row_bytes.len();
            self.data[slot.page_offset..end_of_data].copy_from_slice(&row_bytes);
            Ok(())
        } else {
            Err(PageError::UpdateRecord)
        }
    }

    /// Inserts the record into the page and returns the slot index of the inserted record
    pub fn insert_record(&mut self, row_bytes: Vec<u8>) -> Result<usize, PageError> {
        if !self.can_insert(&row_bytes) {
            return Err(PageError::InsertRecord);
        }

        let new_record_len = row_bytes.len() as u16; // size already checked
        let slot_index;
        // It's quite expensive and messy to insert directly into deleted slots
        // better: use page compaction regularly and put the dead tuples into the free space area
        // How this could work:
        // - defragment the page and update the slot pointers that are not deleted
        // - don't delete the deleted slot pointer (because that would break the index)
        // - only reuse slot pointers after the index is not using them anymore (do index cleanup regularly)
        let deleted_slot = self
            .find_free_slot_index(&row_bytes)
            .and_then(|slot_index| {
                self.slots
                    .get_mut(slot_index)
                    .map(|slot| (slot_index, slot))
            });

        let (slot_deleted, new_slot_len, slot_data_start_offset) =
            if let Some((index, slot)) = deleted_slot {
                slot_index = index;
                let end_of_data = slot.page_offset + row_bytes.len();
                self.data[slot.page_offset..end_of_data].copy_from_slice(&row_bytes);

                // Need a new slot if the length of the inserted record is not as long as the slot
                let remaining_slot_len = slot.record_length - new_record_len;

                if remaining_slot_len > 0 {
                    slot.record_length = new_record_len;
                }

                slot.deleted = false;

                (true, remaining_slot_len, end_of_data)
            } else {
                // just returning the length of slots should be correct
                // because a new slot must be allocated for this data
                slot_index = self.slots.len();
                let start_of_data = self.data_offset - row_bytes.len();
                self.data[start_of_data..self.data_offset].copy_from_slice(&row_bytes);
                self.data_offset -= row_bytes.len();

                (false, row_bytes.len() as u16, start_of_data)
            };

        if new_slot_len > 0 {
            self.allocate_slot(slot_data_start_offset, new_slot_len, slot_deleted);
        }

        self.number_of_records += 1;

        Ok(slot_index)
    }

    fn allocate_slot(&mut self, page_offset: usize, record_length: u16, deleted: bool) {
        // It's possible to allocate deleted slot
        // for example, if a deleted slot has been overwritten with a smaller record,
        // the remaining free space can be allocated as a new slot
        self.slots.push(Slot {
            record_length,
            page_offset,
            deleted,
        });
    }

    pub fn delete_record(&mut self, record_index: usize) -> bool {
        let slot = self.slots.get_mut(record_index);

        if let Some(slot) = slot {
            slot.deleted = true;
            true
        } else {
            // Proper error handling?
            false
        }
    }

    fn read_data(&self, slot: &Slot) -> &[u8] {
        &self.data[slot.page_offset..slot.page_offset + slot.record_length as usize]
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = vec![0u8; self.layout.page_size()];
        // Number of rows 2 Bytes
        buf[PageDataLayout::INDEX_NUMBER_ROWS..PageDataLayout::INDEX_ROW_OFFSET]
            .copy_from_slice(&self.number_of_records.to_be_bytes());

        // Offset 4 Bytes
        let offset_bytes = (self.data_offset as u32).to_be_bytes();
        buf[PageDataLayout::INDEX_ROW_OFFSET..PageDataLayout::INDEX_PAGE_ID]
            .copy_from_slice(&offset_bytes);

        // PageId 4 Bytes
        let page_id_bytes = self.page_id.to_be_bytes();
        buf[PageDataLayout::INDEX_PAGE_ID..PageDataLayout::INDEX_FREE_SLOTS_OFFSET]
            .copy_from_slice(&page_id_bytes);

        // Free slots offset 4 Bytes
        let free_slots_offset_bytes =
            ((self.slots.len() * PageDataLayout::SLOT_SIZE) as i32).to_be_bytes();
        buf[PageDataLayout::INDEX_FREE_SLOTS_OFFSET..PageDataLayout::INDEX_FREE_SLOTS_OFFSET + 4]
            .copy_from_slice(&free_slots_offset_bytes);

        buf[PageDataLayout::INDEX_FREE_SLOTS_OFFSET + 4..self.layout.page_size()]
            .copy_from_slice(&self.data);

        // serialize Slots:
        for (i, slot) in self.slots.iter().enumerate() {
            let deleted_flag =
                PageDataLayout::INDEX_FREE_SLOTS_START + (i * PageDataLayout::SLOT_SIZE);
            let offset_of_data_index = deleted_flag + 1;
            let length_index = deleted_flag + PageDataLayout::SLOT_RECORD_LENGTH_INDEX;
            let length_end = length_index + 2;

            // deleted flag
            buf[deleted_flag] = if slot.deleted { 1 } else { 0 };
            // offset
            buf[offset_of_data_index..length_index]
                .copy_from_slice(&(slot.page_offset as u32).to_be_bytes());
            // length
            buf[length_index..length_end].copy_from_slice(&(slot.record_length).to_be_bytes());
        }

        buf
    }

    pub fn deserialize(buf: &[u8], layout: &'database PageDataLayout) -> Result<Self, PageError> {
        let num_rows = u16::from_be_bytes(
            buf[PageDataLayout::INDEX_NUMBER_ROWS..PageDataLayout::INDEX_ROW_OFFSET].try_into()?,
        );

        let offset = i32::from_be_bytes(
            buf[PageDataLayout::INDEX_ROW_OFFSET..PageDataLayout::INDEX_PAGE_ID].try_into()?,
        );

        let page_id = i32::from_be_bytes(
            buf[PageDataLayout::INDEX_PAGE_ID..PageDataLayout::INDEX_FREE_SLOTS_OFFSET]
                .try_into()?,
        );

        let free_slots_offset = i32::from_be_bytes(
            buf[PageDataLayout::INDEX_FREE_SLOTS_OFFSET
                ..PageDataLayout::INDEX_FREE_SLOTS_OFFSET + 4]
                .try_into()?,
        ) as usize;

        let data = buf[PageDataLayout::INDEX_FREE_SLOTS_OFFSET + 4..layout.page_size()].to_vec();

        let free_slots: Vec<Slot> = data[0..free_slots_offset]
            .to_vec()
            .chunks_exact(7)
            .map(|chunk| {
                let deleted = chunk[0] == 1;
                let offset = u32::from_be_bytes(chunk[1..5].try_into()?) as usize;
                let length = u16::from_be_bytes(chunk[5..7].try_into()?);
                let slot = Slot {
                    page_offset: offset,
                    record_length: length,
                    deleted,
                };
                Ok(slot)
            })
            .collect::<Result<Vec<Slot>, TryFromSliceError>>()?;

        Ok(Self {
            layout,
            number_of_records: num_rows,
            data_offset: offset as usize,
            page_id,
            data,
            slots: free_slots,
            slots_offset: free_slots_offset,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::data::page::{Page, PageDataLayout, PageDataLayoutError};

    #[test]
    fn should_insert_new_data_in_deleted_slot_if_it_fits() {
        let layout = PageDataLayout::new(64).unwrap();
        let mut page = Page::new(&layout);

        page.insert_record(vec![1, 2, 1, 2]).unwrap();
        page.insert_record(vec![2, 4, 2, 4]).unwrap();
        page.insert_record(vec![3, 5, 3, 5]).unwrap();

        page.delete_record(1);
        page.insert_record(vec![9, 9, 9]).unwrap();

        // A new slot with size 1 has been created, because the new record didn't fit perfectly
        assert_eq!(page.slots.len(), 4);
        assert_eq!(page.slots[3].deleted, true);

        page.insert_record(vec![10]).unwrap();
        assert_eq!(page.slots[3].deleted, false);
        assert_eq!(page.read_data(&page.slots[3]), [10]);

        let record = page
            .record_iterator()
            .find(|v| v.data.data()[0] == 9)
            .unwrap();
        assert_eq!(record.data.data(), &[9, 9, 9]);
        assert_eq!(record.record_index, 1);
    }

    #[test]
    fn should_update_record_in_place() {
        let layout = PageDataLayout::new(64).unwrap();
        let mut page = Page::new(&layout);

        page.insert_record(vec![1, 2, 1, 2]).unwrap();
        page.insert_record(vec![2, 4, 2, 4]).unwrap();
        page.insert_record(vec![3, 5, 3, 5]).unwrap();

        let ser_page = page.serialize();
        let record = page
            .record_iterator()
            .find(|v| v.data.data()[0] == 2)
            .unwrap();

        let mut page = Page::deserialize(&ser_page, &layout).unwrap();
        page.write_record(record.record_index, vec![2, 2, 2, 2])
            .unwrap();
        let record = page
            .record_iterator()
            .find(|v| v.data.data()[0] == 2)
            .unwrap();
        assert_eq!(record.data.data(), &[2, 2, 2, 2]);
        assert_eq!(record.record_index, 1);
    }

    #[test]
    fn should_not_allow_page_layout_size_less_than_32() {
        let result = PageDataLayout::new(31);
        assert!(result.is_err());
        matches!(result.err().unwrap(), PageDataLayoutError::InvalidPageSize);
    }

    #[test]
    fn should_allow_page_layout_size_greater_than_31() {
        let result = PageDataLayout::new(32);
        assert!(result.is_ok());
    }

    #[test]
    fn should_calc_all_values_correctly_when_insert_row() {
        let layout = PageDataLayout::new(32).unwrap();
        let mut page = Page::new(&layout);

        // insert 7 bytes
        let row = vec![1, 2, 3, 4, 5, 6, 7];
        page.insert_record(row.clone()).unwrap();

        // 32 - 14(header) = 18
        assert_eq!(page.data.len(), 18);
        assert_eq!(page.slots.len(), 1);
        //points to offset 11, but no free space

        let slot_option = page.slots.get(0);
        assert!(slot_option.is_some());
        let slot = slot_option.unwrap();
        assert_eq!(slot.deleted, false);
        assert_eq!(slot.record_length, 7);
        assert_eq!(slot.page_offset, 11);

        assert_eq!(page.row_data_size(), 7);
        let data = page.row_data();
        assert_eq!(data, row);
    }

    #[test]
    fn should_serialize_and_deserialize_correctly() {
        let layout = PageDataLayout::new(32).unwrap();
        let mut page = Page::new(&layout);
        page.set_page_id(1);

        // insert 7 bytes
        let row = vec![1, 2, 3, 4, 5, 6, 7];
        page.insert_record(row.clone()).unwrap();

        // act: serialize
        let bytes = page.serialize();

        let deserialized_page = Page::deserialize(&bytes, &layout).unwrap();

        assert_eq!(deserialized_page.page_id, 1);
        // 32 - 14(header) = 18
        assert_eq!(deserialized_page.data.len(), 18);
        assert_eq!(deserialized_page.slots.len(), 1);
        // Slot points to offset 11 and it isn't deleted
        let slot_option = deserialized_page.slots.get(0);
        assert!(slot_option.is_some());
        let slot = slot_option.unwrap();
        assert_eq!(slot.deleted, false);
        assert_eq!(slot.record_length, 7);
        assert_eq!(slot.page_offset, 11);

        assert_eq!(deserialized_page.row_data_size(), 7);
        let data = deserialized_page.row_data();
        assert_eq!(data, row);
    }

    #[test]
    fn should_serialize_and_deserialize_correctly_multiple_inserts() {
        let layout = PageDataLayout::new(64).unwrap();
        let mut page = Page::new(&layout);
        page.set_page_id(1);

        page.insert_record(vec![1, 2, 3]).unwrap();
        page.insert_record(vec![4, 5, 6]).unwrap();
        page.insert_record(vec![7, 8, 9]).unwrap();

        // act: serialize + deserialize
        let bytes = page.serialize();
        let deserialized_page = Page::deserialize(&bytes, &layout).unwrap();

        assert_eq!(deserialized_page.page_id, 1);
        // 64 - 14(header) = 50
        assert_eq!(deserialized_page.data.len(), 50);
        assert_eq!(deserialized_page.slots.len(), 3);

        let slot_option = page.slots.get(2);
        assert!(slot_option.is_some());
        let slot = slot_option.unwrap();
        assert_eq!(slot.deleted, false);
        assert_eq!(slot.record_length, 3);
        // 50 - 9 = 41
        assert_eq!(slot.page_offset, 41);

        assert_eq!(deserialized_page.row_data_size(), 9);
    }

    #[test]
    fn data_iterator_should_return_none_if_no_data_present() {
        let layout = PageDataLayout::new(32).unwrap();
        let page = Page::new(&layout);
        let record_data = page.record_iterator().next();

        assert!(record_data.is_none());
    }

    #[test]
    fn data_iterator_should_iterate_over_record_data() {
        let layout = PageDataLayout::new(64).unwrap();
        let mut page = Page::new(&layout);
        page.insert_record(vec![1, 2, 3]).unwrap();
        page.insert_record(vec![4, 5, 6]).unwrap();

        let mut rec_iterator = page.record_iterator();
        let mut record_data = rec_iterator.next();
        assert!(matches!(record_data, Some(record) if record.data() == &[1, 2, 3]));

        record_data = rec_iterator.next();
        assert!(matches!(record_data, Some(record) if record.data() == &[4, 5, 6]));

        record_data = rec_iterator.next();
        assert!(record_data.is_none());
    }

    #[test]
    fn data_iterator_should_not_return_deleted_data() {
        let layout = PageDataLayout::new(64).unwrap();
        let mut page = Page::new(&layout);
        page.insert_record(vec![1, 2, 3]).unwrap();
        page.insert_record(vec![4, 5, 6]).unwrap();
        page.slots[0].deleted = true;

        let mut rec_iterator = page.record_iterator();
        let mut record_data = rec_iterator.next();
        assert!(matches!(record_data, Some(record) if record.data() == &[4, 5, 6]));

        record_data = rec_iterator.next();
        assert!(record_data.is_none());
    }
}
