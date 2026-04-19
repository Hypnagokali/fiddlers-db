/// Most of the code is taken from or heavily inspired by PostgreSQL: 
/// https://github.com/postgres/postgres/tree/master/src/backend/storage/freespace
/// It's a bit modified, so that it fits the learning purpose
use core::panic;

use crate::{data::page::{self, Page, PageDataLayout}, fsm::binary_tree::BinaryTree, store::{PageStorage, Store}, table::table::Table};

struct FsmAccess<'db> {
    table: &'db Table,
}

impl FsmAccess<'_> {
    pub fn new(table: &Table) -> FsmAccess<'_> {
        FsmAccess { table }
    }
    pub fn table(&self) -> &Table {
        self.table
    }
}

impl PageStorage for FsmAccess<'_> {
    fn file_path(&self) -> String {
        format!("fsm_{}.dat", self.table().id())
    }
}

#[derive(PartialEq, Debug)]
struct FsmAddress {
    page_number: usize, // logical page number at that level
    level: usize,
}

pub struct Fsm<'db, S: Store> {
    store: &'db S,
    table: &'db Table,
    page_layout: &'db PageDataLayout,
    root_level: usize,
    fsm_category_step: usize,
    root_addr: FsmAddress,
    slots: usize,
}

impl<'db, S: Store> Fsm<'db, S> {

    pub fn new(store: &'db S, page_layout: &'db PageDataLayout, table: &'db Table) -> Self {
        if page_layout.page_size() < 256 {
            panic!("Page size is too small. 256 is the minimum page size");
        }

        let step_size = page_layout.page_size() / 256;

        // Since page IDs are i32 without 0, the tree must be able to address 2^31 - 1 pages.
        // 1291 as the smallest number that satisfies X^3 >= 2^31 - 1. That means 3 levels from 1291 slots on, 
        // and that are 2581 nodes + 21 bytes header (and 1 slot allocation) = 2602 (practically 4096, because only page sizes of a power of 2 are allowed)
        // 216 is the smallest number that satisfies X^4 >= 2^31 - 1. That means 4 levels from 216 slots on.
        // The implementation will only support 3 or 4 levels, if the page size is smaller than 512 bytes, not every page can be addressed.
        let depth = if page_layout.page_size() >= 4096 {
            3
        } else {
            4
        };

        let root_addr = FsmAddress {
            page_number: 0,
            level: depth - 1,
        };

        let structure = BinaryTree::node_structure(page_layout);

        Self {
            store,
            table,
            page_layout,
            root_level: depth - 1, // for 3 levels there are: 2 (root) -> 1 (internal) -> 0 (leaf)
            fsm_category_step: step_size,
            root_addr,
            slots: structure.leaf_nodes,
        }
    }

    /// Returns a page with enough free space, if there isn't any, it allocates a new page.
    pub fn find_available_page(&self, bytes: usize) -> Page<'db> {
        // start at root:
        let addr = &self.root_addr;

        let access = FsmAccess {
            table: self.table,
        };

        // Easiest is, to allocate a page if needed (violates the separation of concerns a bit, but it can be refactored later)

        // 1. Translate addr to a page_id
        // 2. Read meta data and check if the page exists
        // 3. if not, create the page and all heap pages

        let page = self.store.read_page(self.page_layout, 1, &access);

        unimplemented!()
    }

    pub fn update_available_space(&self, page: Page<'_>) {
        unimplemented!()
    }

    fn page_space_to_category(&self, available: usize) -> u8 {
        // Round down: category 0 is treated as no space left.
        // For a page size of 8kb 0-31 bytes are treated as 0

        // Only a new fresh page goes into 255
        if available >= self.page_layout.max_tupel_size() {
            return 255;
        }

        let cat = available / self.fsm_category_step;

        if cat > 254 {
            // The highest category is reserved for new pages
            254
        } else {
            cat as u8
        }
    }

    fn space_needed_to_category(&self, needed: usize) -> u8 {
        // If there is more space requested than a page can provide,
        // The Page itself will complain, that there is not enough space.

        // Round up: 0-31 bytes will go in category 1
        if needed == 0 {
            return 1;
        }

        let cat = (needed as f32 / self.fsm_category_step as f32).ceil() as usize;

        if cat > 255 {
            255
        } else {
            cat as u8
        }
    }

    // Returns FsmAddress and slot
    fn heap_page_id_to_addr(&self, page_id: i32) -> (FsmAddress, usize) {
        let slot_index_globally = page_id - 1;
        if slot_index_globally < 0 {
            // TODO: replace with proper error handling
            panic!("Slot index cannot be less than 0 (page_id < 1 not valid)");
        }
        let logical_fsm_page = slot_index_globally as usize / self.slots;
        let slot_on_fsm_page = slot_index_globally as usize - logical_fsm_page * self.slots;

        let addr = FsmAddress {
            page_number: logical_fsm_page,
            level: 0,
        };

        (addr, slot_on_fsm_page)
    }

    fn addr_slot_to_heap_page_id(&self, addr: &FsmAddress, slot_index: usize) -> i32 {
        let slot_index_globally = addr.page_number * self.slots + slot_index;

        return slot_index_globally as i32 + 1;
    }

    fn logical_addr_to_fms_page_id(&self, addr: &FsmAddress) -> i32 {
        // The pages are constructed from root to bottom and then the leaves
        // Page ID 1 is always the root
        // ID 2 is always the first child of the root
        // ID 3 (if 3 level tree) is the first leaf, 4 the second, 5 the third, etc...
        // For a tree with 4 leaves the physical structure is: L2 (root), L1, L0, L0, L0, L0, L1, L0, L0, L0, L0, L1, L0, ... L2, L1, L0, ....
        // So, all nodes before the current node (addr) must be counted:
        let mut leaf_number = addr.page_number;
        let lvl = addr.level;

        // Calc logical page number of the first (bottom) leaf page of this node  
        for l in 0..lvl {
            leaf_number *= self.slots;
        }

        let mut pages = 0;
        // count pages from bottom to top (this includes the children of the target node, they will be subtracted later)
        for l in 0..self.root_level {
            pages += leaf_number + 1; // page on position 0 is 1 page
            leaf_number /= leaf_number / self.slots; // get the next position one level up
        }

        // subtracts the extra nodes
        pages -= addr.level;

        // There is at least one other method to count the nodes:
        // go upwards by dividing by the number of slots and rounding up
        // go downwards by multiplying with number of slots      

        (pages - 1) as i32 // to get the 0 based position
    }

}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{data::page::PageDataLayout, fsm::{binary_tree::BinaryTree, fsm::FsmAccess}, store::{Store, file_store::FileStore}, table::{Column, ColumnType, TableSchema, table::Table}};

    #[test]
    fn should_read_binary_tree_from_page() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(512).unwrap();

        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);

        let table = Table::new(1, "test".to_owned(), schema);
        let fsm_access = FsmAccess::new(&table);
        store.create(&layout, &fsm_access).unwrap();
        // init tree
        let mut tree = BinaryTree::new(&layout);
        tree.set_available_space(199, 20);
        tree.set_available_space(200, 200);
        tree.set_available_space(201, 240);

        let mut page = store.allocate_page(&layout, &fsm_access).unwrap();
        page.insert_record(tree.serialize()).unwrap();
        store.write_page(&layout, &mut page, &fsm_access).unwrap();

        let mut page = store.read_page(&layout, 1, &fsm_access).unwrap();
        let tree_bytes = page.read_slot(0).unwrap();
        let tree = BinaryTree::deserialize(tree_bytes, &layout);

        let slot = tree.find_available(18);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().slot_index(), 199);

        let slot = tree.find_available(202);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().slot_index(), 201);
    }
}