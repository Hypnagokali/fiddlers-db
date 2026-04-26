use crate::{data::page::{self, PageDataLayout}, tree};

/// This is highly inspired by Postgres' FSM implementation:
/// https://github.com/postgres/postgres/blob/master/src/backend/storage/freespace/fsmpage.c

pub struct BinaryTree {
    // an reference into the page would be more efficient
    arr: Vec<u8>,
    non_leaf_nodes: usize,
    leaf_nodes: usize,
}

pub struct NodeStructure {
    pub leaf_nodes: usize,
    pub non_leaf_nodes: usize,
    pub nodes: usize,
}

#[derive(Debug)]
pub struct FsmSlot {
    slot_index: usize,
    available_space: u8,
}

impl FsmSlot {
    pub fn slot_index(&self) -> usize {
        self.slot_index
    }

    pub fn available_space(&self) -> u8 {
        self.available_space
    }
}

fn left_child(x: usize) -> usize {
    2 * x + 1
}
fn right_child(x: usize) -> usize {
    2 * x + 2
}
fn parent(x: usize) -> usize {
    (x - 1) / 2
}

impl BinaryTree {
    pub fn node_structure(page_layout: &PageDataLayout) -> NodeStructure {
        if page_layout.page_size() < 64 {
            // Such small page sizes are only relevant for testing.
            panic!("Page size is too small to fit the binary tree. It needs at least 64 bytes");
        }
        let nodes = page_layout.page_data_size() - PageDataLayout::SLOT_SIZE as usize;
        let non_leaf_nodes = page_layout.page_size() / 2 - 1;
        let leaf_nodes = nodes - non_leaf_nodes;

        NodeStructure { leaf_nodes, non_leaf_nodes, nodes }
    }

    pub fn new(page_layout: &PageDataLayout) -> Self {
        let structure = BinaryTree::node_structure(page_layout);

        let arr = vec![0; structure.nodes];
        Self { 
            arr, 
            non_leaf_nodes: structure.non_leaf_nodes,
            leaf_nodes: structure.leaf_nodes,
        }
    }

    pub fn deserialize(bytes: &[u8], page_layout: &PageDataLayout) -> Self {
        let max = bytes.iter().max();
        let mut tree = BinaryTree::new(page_layout);
        
        if tree.arr.len() != bytes.len() {
            panic!("Invalid byte array length for deserialization. PageLayout is maybe incompatible");
        }
        
        // this is overhead (ref directly into the page array would be nicer)
        tree.arr = bytes.to_vec();

        tree
    }

    pub fn serialize(self) -> Vec<u8> {
        self.arr
    }

    pub fn find_available(&self, min_space_needed: u8) -> Option<FsmSlot> {
        // This is a very naive implementation. It fills the heap pages from top to bottom (prefers the left most node)
        if self.arr[0] < min_space_needed {
            return None;
        }

        let mut current = 0;
        while !self.is_leaf_node(current) {
            let left_child_index = left_child(current);
            let right_child_index = left_child_index + 1;

            let left_child_value = self.arr[left_child_index];
            let right_child_value = if right_child_index < self.arr.len() {
                self.arr[right_child_index]
            } else {
                0
            };

            if left_child_value >= min_space_needed {
                current = left_child_index;
            } else if right_child_value >= min_space_needed {
                current = right_child_index;
            } else {
                // This should never be reached, otherwise the tree is corrupted
                return None; 
            }
        }

        let slot = current - self.non_leaf_nodes;

        Some(FsmSlot { 
            slot_index: slot,
            available_space: self.arr[slot],
        })
    }

    pub fn set_available_space(&mut self, slot: usize, available_space: u8) {        
        // assert slot < leaf nodes
        if self.leaf_nodes <= slot {
            // Proper error handling or should this actually never happen?
            panic!("slot index is out of bounds");
        }

        let mut node_number = self.non_leaf_nodes + slot;
        if node_number >= self.arr.len() {
            return;
        }

        // If the value is the same as before, we can skip the update and traversal
        // But if the root is not greater than the new available space, we need to correct it
        if self.arr[node_number] == available_space && available_space <= self.arr[0] {
            return;
        }

        self.arr[node_number] = available_space;

        loop {
            // update parent node
            node_number = parent(node_number);
            let left_child_index = left_child(node_number);
            let right_child_index = left_child_index + 1;

            let mut new_value = self.arr[left_child_index];

            if right_child_index < self.arr.len() {
                // if right value is higher take that one
                 new_value = new_value.max(self.arr[right_child_index]);
            }

            let old_value = self.arr[node_number];

            if old_value == new_value {
                break;
            }

            self.arr[node_number] = new_value;

            if node_number == 0 {
                break;
            }
        }
    }

    fn is_leaf_node(&self, index: usize) -> bool {
        // >= because of the mapping from count to index
        index >= self.non_leaf_nodes
    }

    #[cfg(test)]
    pub fn debug(&self) {
        let max_val = self.arr.iter().max().unwrap_or(&0);
        println!("Tree has max value: {}", max_val);
    }
}

#[cfg(test)]
mod tests {
    use crate::{data::page::PageDataLayout, fsm::binary_tree::{BinaryTree, left_child, parent, right_child}};

    #[test]
    fn should_find_middle_leaf() {
        let layout = PageDataLayout::new(512).unwrap();
        // results in 236 leaf nodes
        let mut tree = BinaryTree::new(&layout);
        tree.set_available_space(0, 20);
        tree.set_available_space(10, 100);
        tree.set_available_space(30, 200);
        tree.set_available_space(150, 5);
        tree.set_available_space(200, 5);
        tree.set_available_space(235, 20);

        let slot = tree.find_available(200);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().slot_index, 30);
    }

    #[test]
    fn should_find_right_most_leaf() {
        let layout = PageDataLayout::new(512).unwrap();
        // results in 236 leaf nodes
        let mut tree = BinaryTree::new(&layout);
        tree.set_available_space(0, 20);
        tree.set_available_space(10, 100);
        tree.set_available_space(40, 10);
        tree.set_available_space(200, 5);
        tree.set_available_space(235, 200);

        let slot = tree.find_available(200);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().slot_index, 235);
    }

    #[test]
    fn should_find_left_most_leaf() {
        let layout = PageDataLayout::new(512).unwrap();
        // results in 236 leaf nodes
        let mut tree = BinaryTree::new(&layout);
        tree.set_available_space(100, 100);
        tree.set_available_space(40, 10);
        tree.set_available_space(5, 5);
        tree.set_available_space(230, 20);
        tree.set_available_space(2, 20);
        tree.set_available_space(1, 20);
        tree.set_available_space(0, 200);

        let slot = tree.find_available(200);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().slot_index, 0);
    }

    #[test]
    fn should_find_always_left_most_that_fits() {
        let layout = PageDataLayout::new(512).unwrap();
        // results in 236 leaf nodes
        let mut tree = BinaryTree::new(&layout);
        tree.set_available_space(0, 2);
        tree.set_available_space(1, 20);
        tree.set_available_space(5, 5);
        tree.set_available_space(40, 10);
        tree.set_available_space(100, 100);
        tree.set_available_space(230, 20);

        let slot = tree.find_available(5);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().slot_index, 1);
        tree.set_available_space(1, 6);

        // Slot 1 has no longer enough space for the next request
        let slot = tree.find_available(10);
        assert!(slot.is_some());
        assert_eq!(slot.unwrap().slot_index, 40);
    }


    #[test]
    fn should_traverse_tree() {
        // nodes are named: 0, 1, 2, ...
        // right node from root is 2:
        assert_eq!(right_child(0), 2);
        assert_eq!(left_child(0), 1);
        // right node from 1 is 4:
        assert_eq!(right_child(1), 4);
        assert_eq!(left_child(1), 3);
        // right node from 4 is 10:
        assert_eq!(right_child(4), 10);
        assert_eq!(left_child(4), 9);

        // parent of 9 is 4:
        assert_eq!(parent(9), 4);
        // parrent of 4 is 1:
        assert_eq!(parent(4), 1);
        // parent of 1 is 0:
        assert_eq!(parent(1), 0);
    }

}