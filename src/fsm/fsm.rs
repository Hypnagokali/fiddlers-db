//! Most of the code is taken from PostgreSQL with some modifications (e.g., use floats and rounding instead of integer division)
//! https://github.com/postgres/postgres/tree/master/src/backend/storage/freespace
//! Focuses on understandability.
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
    node_number: usize, // logical node number at that level
    level: usize,
}

// Implemented with a lot of trust
// No self healing, no check, if the page can be read as BinaryTree
// If FSM is corrupted, the database will likely explode
pub struct Fsm<'db, S> {
    store: &'db S,
    table: &'db Table,
    page_layout: &'db PageDataLayout,
    root_level: usize,
    fsm_category_step: usize,
    depth: usize,
    slots: usize,
}

impl Fsm<'_, ()> {
    pub fn access<'a>(table: &'a Table) -> impl PageStorage + 'a {
        FsmAccess::new(table)
    }
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

        let structure = BinaryTree::node_structure(page_layout);

        Self {
            store,
            table,
            page_layout,
            root_level: depth - 1, // for 3 levels there are: 2 (root) -> 1 (internal) -> 0 (leaf)
            fsm_category_step: step_size,
            depth,
            slots: structure.leaf_nodes,
        }
    }
    fn root(&self) -> FsmAddress {
        // create a owned FsmAddress, so it's easier to walk the tree using the same variable
        FsmAddress {
            node_number: 0,
            level: self.depth - 1,
        }
    }

    /// Updates the FSM tree for this page
    pub fn update(&self, page: &Page<'_>) {
        let free_space = page.max_available_space();
        let free_space_cat = self.page_space_to_category(free_space);

        let (mut addr, mut slot) = self.heap_page_id_to_addr(page.page_id());
        let access = FsmAccess {
            table: self.table,
        };


        let mut fsm_page_id = self.logical_addr_to_fms_page_id(&addr);
        // check if fsm pages exist
        let meta = self.store.read_metadata(self.page_layout, &access).unwrap();
        if meta.next_id() <= fsm_page_id {
            for _ in meta.next_id()..fsm_page_id + 1 {
                self.allocate_new_fsm_and_init(&access);                   
            }
        }

        let root = self.root();
        for level in 0..self.depth {
            let mut fsm_page = self.store.read_page(self.page_layout, fsm_page_id, &access).unwrap();
            let mut fsm_tree = BinaryTree::deserialize(
                fsm_page.read_slot(0).unwrap(), // TODO: proper error handling
                self.page_layout
            );

            fsm_tree.set_available_space(slot, free_space_cat);
            fsm_page.write_record(0, fsm_tree.serialize());
            self.store.write_page(self.page_layout, &fsm_page, &access).unwrap();          

            if level < root.level {
                (addr, slot) = self.parent(&addr);
                fsm_page_id = self.logical_addr_to_fms_page_id(&addr);
            }
        }

    }

    /// Returns a page with enough free space, if there isn't any, it allocates a new page.
    /// If root points to a page with enough space, it's expected, that the tree is initialized up to this point
    /// If not, just return the page. The tree will be initialized when using the `update` method
    pub fn find_available_page(&self, bytes: usize) -> Page<'db> {
        // start at root:
        let mut addr = self.root();
        let cat = self.space_needed_to_category(bytes);

        let access = FsmAccess {
            table: self.table,
        };

        // Easiest is, to allocate a page if needed (violates the separation of concerns a bit, but it can be refactored later)

        // Init
        let meta_data = self.store.read_metadata(self.page_layout, &access).unwrap();
        let num_pages = meta_data.number_of_pages() as usize;
        for _ in num_pages..self.root_level + 1 {
            // init tree to full size if not already
            self.allocate_new_fsm_and_init(&access);
        }

        // if the root points to enough space, the next iterations must not fail
        // this is a quick invariant to check, if FSM is corrupted or not.
        let mut should_never_fail = false;

        let mut heap_page_id = 0;
        for _ in 0..self.depth  {
            // loop without self healing
            // translate to physical address (page_id)
            let page_id = self.logical_addr_to_fms_page_id(&addr);
            let fsm_page = self.store.read_page(self.page_layout, page_id, &access).unwrap();
            let fsm_tree = BinaryTree::deserialize(
                fsm_page.read_slot(0).unwrap(), // TODO: proper error handling
                self.page_layout
            );

            let slot = fsm_tree.find_available(cat);
            
            if let Some(slot) = slot {
                if addr.level == 0 {
                    // bottom level, find heap page
                    heap_page_id = self.addr_slot_to_heap_page_id(&addr, slot.slot_index());
                } else {
                    // navigate to child
                    addr = self.child(&addr, slot.slot_index());
                    should_never_fail = true;
                }
            } else {
                if should_never_fail {
                    // TODO: proper error handling
                    panic!("FSM corrupted. Must always find a valid address after root pointed to enough space.");
                }
                // need new page
                return self.store.allocate_page(&self.page_layout, self.table).unwrap();
            }

        }

        if heap_page_id < 1 {
            // TODO: proper error handling
            panic!("No page found. Corrupted FSM");
        }

        // TODO: Last check: does the page_id exist
        // TODO: Proper error handling
        self.store.read_page(self.page_layout, heap_page_id, self.table).unwrap()
    }

    // Returns parent FsmAddress and slot in parent
    fn parent(&self, child_addr: &FsmAddress) -> (FsmAddress, usize) {
        if child_addr.level == self.root_level {
            // TODO: proper error handling
            panic!("Try to reach parent from root level");
        }

        let level = child_addr.level + 1;
        let node_number = child_addr.node_number / self.slots;
        let parent_slot = child_addr.node_number % self.slots; // map to 0...slots - 1

        let parent = FsmAddress {
            node_number,
            level,
        };

        (parent, parent_slot)
    }

    fn child(&self, parent_addr: &FsmAddress, slot: usize) -> FsmAddress {
        if parent_addr.level == 0 {
            // TODO: proper error handling
            panic!("Try to reach a child from bottom level");
        }

        let level = parent_addr.level - 1;
        let node_number = parent_addr.node_number * self.slots + slot;

        FsmAddress {
            node_number,
            level,
        }
    }

    fn allocate_new_fsm_and_init(&self, access: &FsmAccess) {
        let mut page = self.store.allocate_page(self.page_layout, access).unwrap();
        let binary_tree = BinaryTree::new(self.page_layout);
        page.insert_record(binary_tree.serialize()).unwrap();
        self.store.write_page(self.page_layout, &page, access).unwrap();
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

        // Round up: even 0 bytes to category 1
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
            node_number: logical_fsm_page,
            level: 0,
        };

        (addr, slot_on_fsm_page)
    }

    fn addr_slot_to_heap_page_id(&self, addr: &FsmAddress, slot_index: usize) -> i32 {
        if addr.level > 0 {
            // TODO: proper error handling
            panic!("No heap pages linked on level: {}", addr.level);
        }
        let slot_index_globally = addr.node_number * self.slots + slot_index;

        return slot_index_globally as i32 + 1;
    }

    fn logical_addr_to_fms_page_id(&self, addr: &FsmAddress) -> i32 {
        // The pages are constructed from root to bottom and then the leaves
        // Page ID 1 is always the root
        // ID 2 is always the first child of the root
        // ID 3 (if 3 level tree) is the first leaf, 4 the second, 5 the third, etc...
        // For a tree with 4 leaves the physical structure is: L2 (root), L1, L0, L0, L0, L0, L1, L0, L0, L0, L0, L1, L0, ... L2, L1, L0, ....
        // So, all nodes before the current node (addr) must be counted:
        let mut leaf_number = addr.node_number;
        let lvl = addr.level;

        // Calc logical page number of the first (bottom) leaf page of this node  
        for l in 0..lvl {
            leaf_number *= self.slots;
        }

        let mut pages = 0;
        // count pages from bottom to top (this includes the children of the target node, they will be subtracted later)
        for l in 0..self.depth {
            pages += leaf_number + 1; // page on position 0 is 1 page
            leaf_number /= self.slots; // get the next position one level up
        }

        // subtracts the extra nodes
        pages -= addr.level;

        // There is at least one other method to count the nodes:
        // go upwards by dividing by the number of slots and rounding up
        // go downwards by multiplying with number of slots      

        pages as i32 // page_ids start at 1
    }

}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use crate::{data::page::PageDataLayout, fsm::{binary_tree::BinaryTree, fsm::{Fsm, FsmAccess, FsmAddress}}, store::{Store, file_store::FileStore}, table::{Column, ColumnType, TableSchema, table::Table}};

    #[test]
    fn should_walk_the_tree_downwards() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(256).unwrap();
        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);
        let table = Table::new(1, "test".to_owned(), schema);
        let fsm = Fsm::new(&store, &layout, &table);

        let parent_addr = FsmAddress {
            node_number: 0,
            level: 3,
        };

        // Get child logical address: 
        // is just the slot: 1
        let addr = fsm.child(&parent_addr, 1);

        assert_eq!(addr.level, 2);
        assert_eq!(addr.node_number, 1);

        // Get child logical address: 
        // 108 * 1 + 77 = 185
        let addr = fsm.child(&addr, 77);

        assert_eq!(addr.level, 1);
        assert_eq!(addr.node_number, 185);

        // Get child logical address: 
        // 108 * 185 + 20 = 20,000
        let addr = fsm.child(&addr, 20);

        assert_eq!(addr.level, 0);
        assert_eq!(addr.node_number, 20_000);
    }

    #[test]
    fn should_walk_the_tree_upwards() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(256).unwrap();
        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);
        let table = Table::new(1, "test".to_owned(), schema);
        let fsm = Fsm::new(&store, &layout, &table);

        let child_addr = FsmAddress {
            node_number: 20_000,
            level: 0,
        };

        // Get parent logical address: 20,000 / 108 = 185
        // Slot is 2000 % 108 = 56
        let (addr, slot) = fsm.parent(&child_addr);

        assert_eq!(addr.level, 1);
        assert_eq!(addr.node_number, 185);
        assert_eq!(slot, 20);

        // Get parent logical address: 185 / 108 = 1
        // Slot is 185 % 108 = 77
        let (addr, slot) = fsm.parent(&addr);

        assert_eq!(addr.level, 2);
        assert_eq!(addr.node_number, 1);
        assert_eq!(slot, 77);

        // Get parent logical address: 1 / 108 = 0
        // Slot is 1 % 108 = 1
        let (addr, slot) = fsm.parent(&addr);

        assert_eq!(addr.level, 3);
        assert_eq!(addr.node_number, 0);
        assert_eq!(slot, 1);
    }

    #[test]
    fn should_convert_to_physical_page_id() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(256).unwrap();
        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);
        let table = Table::new(1, "test".to_owned(), schema);
        let fsm = Fsm::new(&store, &layout, &table);

        // check some random addresses
        // Level 2 page 9 (10th node, 0 count):
        // All nodes before: 
        // nodes on same level: 10 (nodes: 0-9)
        // leaf nodes before: 9 * 108^2 = 104,985
        // internal nodes lvl 1: 9 * 108 = 972
        // root: 1
        // = 105959
        let addr = FsmAddress {
            node_number: 9,
            level: 2,
        };
        let page_id = fsm.logical_addr_to_fms_page_id(&addr);
        assert_eq!(page_id, 105959);

        // check some random addresses
        // Level 1 page 5000 (5001th node):
        // All nodes before: 
        // nodes on same level: 5001
        // nodes on level 2:    ceil(5001 / 108) = 47
        // root on level 3:     1
        // leaf nodes before:   5000 * 108 = 540,000
        //                      = 545,049
        let addr = FsmAddress {
            node_number: 5000,
            level: 1,
        };
        let page_id = fsm.logical_addr_to_fms_page_id(&addr);
        assert_eq!(page_id, 545_049);
    }

    #[test]
    fn should_calc_addr_correctly() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(256).unwrap();
        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);
        let table = Table::new(1, "test".to_owned(), schema);
        let fsm = Fsm::new(&store, &layout, &table);

        let addr = FsmAddress {
            node_number: 5000,
            level: 0,
        };

        // just a random heap page:
        // slots of all pages before: 5000 * 108 = 540,000
        // slots of current page: 56
        // ids go starts from 1, so + 1 = 540,057
        let heap_page_id = fsm.addr_slot_to_heap_page_id(&addr, 56);

        assert_eq!(heap_page_id, 540_057);        
    }

    #[test]
    fn should_calc_categories_correctly() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(512).unwrap();
        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);
        let table = Table::new(1, "test".to_owned(), schema);
        
        // init fsm
        let fsm_access = FsmAccess::new(&table);
        store.create(&layout, &fsm_access).unwrap();

        let fsm = Fsm::new(&store, &layout, &table);

        // Recorded space is rounded down
        let cat = fsm.page_space_to_category(0);
        assert_eq!(cat, 0);
        // Space needed is rounded up
        let cat = fsm.space_needed_to_category(0);
        assert_eq!(cat, 1);

        // Step is 2, so 300 bytes go into 150
        let cat = fsm.page_space_to_category(300);
        assert_eq!(cat, 150);
        // Rounded up, so when 301 bytes are needed, it goes into 151:
        let cat = fsm.space_needed_to_category(301);
        assert_eq!(cat, 151);

        // A new page goes always into 255 (512 - 21 (header + slot) = 491)
        let cat = fsm.page_space_to_category(491);
        assert_eq!(cat, 255);

        // 490 goes into a much lower category: 245
        let cat = fsm.page_space_to_category(490);
        assert_eq!(cat, 245);
    }

    #[test]
    fn should_update_tree() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(512).unwrap();
        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);
        let table = Table::new(1, "test".to_owned(), schema);
        // init table
        store.create(&layout, &table).unwrap();

        // init fsm
        let fsm_access = FsmAccess::new(&table);
        store.create(&layout, &fsm_access).unwrap();
        let fsm = Fsm::new(&store, &layout, &table);

        // Need a page with the max amount possible:
        let mut full_page = fsm.find_available_page(491);
        let bytes = [1;491].to_vec();
        full_page.insert_record(bytes);
        fsm.update(&full_page);

        // Find a page with 1 byte
        let mut almost_empty_page = fsm.find_available_page(1);
        let bytes = [1;1].to_vec();
        almost_empty_page.insert_record(bytes);
        fsm.update(&almost_empty_page);

        // Find a page with 100 bytes: should use the almost_empty_page:
        let mut needed_100_byte_page = fsm.find_available_page(100);

        assert_eq!(full_page.page_id(), 1);
        assert_eq!(almost_empty_page.page_id(), 2);
        assert_eq!(needed_100_byte_page.page_id(), 2);
    }


    #[test]
    fn should_init_tree_once_if_first_request() {
        let path = tempdir().unwrap();
        let store = FileStore::new(path.path());
        let layout = PageDataLayout::new(512).unwrap();
        let schema = TableSchema::new(vec![
            Column::new(1, "SomeColumn", ColumnType::Varchar(255)),
        ]);
        let table = Table::new(1, "test".to_owned(), schema);
        // init table
        store.create(&layout, &table).unwrap();

        // init fsm
        let fsm_access = FsmAccess::new(&table);
        store.create(&layout, &fsm_access).unwrap();
        let fsm = Fsm::new(&store, &layout, &table);

        // even a 0 bytes request will init the tree and allocate a page
        let page = fsm.find_available_page(0);
        let meta_fsm = store.read_metadata(&layout, &fsm_access).unwrap();
        assert_eq!(page.page_id(), 1);
        // fsm depth should be 4 because of 512 bytes:
        assert_eq!(fsm.depth, 4);
        // nodes 512 - 21 (header) = 491
        // has been init with 512 / 2 - 1 => 255 (non leaf nodes) => 236 slots
        assert_eq!(fsm.slots, 236);
        assert_eq!(meta_fsm.next_id(), 5); // full height is 4 nodes, next page will have id = 5;

        // update tree
        fsm.update(&page);
        
        // a second request should not allocate new pages:
        let page = fsm.find_available_page(200);
        let meta_fsm = store.read_metadata(&layout, &fsm_access).unwrap();
        assert_eq!(page.page_id(), 1);
        assert_eq!(meta_fsm.next_id(), 5);

    }


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