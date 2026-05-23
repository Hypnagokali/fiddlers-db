//! This is a very simple implementation of a B+ Tree page.
//! Since it is just for analyzing how a database may work internally, a simple integer key index is sufficient.
//! Actually, the B+ Tree should be also stored in the generic page,
//! Usually, a node would be split by page size, not by number of keys, 
//! because a key may have a variable size (strings, composites).
//! See (2026-03-16): https://github.com/postgres/postgres/blob/master/src/backend/access/nbtree/README#L160
use std::{cell::RefCell, mem, rc::Rc};

use derive_getters::Getters;
use thiserror::Error;

use crate::tree::store::{NodePager, NodePagerError};

fn binary_search(key: &i32, keys: &[i32]) -> FindKeyResponse {
    let last_element = keys.len().saturating_sub(1);
    let mut high = last_element;
    let mut low = 0;

    if keys.is_empty()|| key > &keys[last_element] {
        return FindKeyResponse::GreaterThanTheLast(last_element);
    }
    
    let mut mid;
    while (high - low > 8) {
        mid = (low + high) / 2;

        if key == &keys[mid] {
            return FindKeyResponse::Equal(mid);
        } else if key < &keys[mid] {
            high = mid;
        } else {
            low = mid + 1;
        }
    }

    for (i, k) in keys.iter().enumerate().take(high + 1).skip(low) {
        if key < k {
            return FindKeyResponse::LessThan(i);
        } else if key == k {
            return FindKeyResponse::Equal(i);
        }
    }

    // Should never reach this point
    FindKeyResponse::GreaterThanTheLast(last_element)
}

#[derive(Debug)]
enum FindKeyResponse {
    GreaterThanTheLast(usize),
    Equal(usize),
    LessThan(usize)
}

#[derive(Debug, Error)]
pub enum NodeOperationError {
    #[error("Unique key constraint error")]
    TryInsertDuplicate,
    #[error("Unknown Page IO error")]
    PageIOError,
    #[error("Cannot update value of non-existing key")]
    TryUpdateNonExistingKey,
    #[error("The node is corrupted")]
    CorruptedNode,
}

impl From<NodePagerError> for NodeOperationError {
    fn from(_: NodePagerError) -> Self {
        NodeOperationError::PageIOError
    }
}


// The clone method is only used for retrieving a Node from cache.
// It's cheaper than to read from file, but far away from perfect.
// A better alternative would be to wrap an inner Rc,
// but it's going to take a bit of effort, because of the inner mutability.
// So I've put it off for now :)
#[derive(Debug, Getters, Clone)]
pub struct NodePage {
    id: i32,
    deleted: bool,
    next_deleted_page: Option<i32>,
    keys: Vec<i32>,
    children: Vec<i32>, // stores page number (page_id)
    values: Vec<(i32, i32)>, // each item is a tuple of (page_id, slot)
    next_leaf: Option<i32>, // Linked list to next leaf-node (if leaf) 
    max_degree: usize,
    changed: Rc<RefCell<bool>>, // flag is not stored, indicates, if the node has been changed
}

impl NodePage {
    pub fn keys_mut(&mut self) -> &mut Vec<i32> {
        *self.changed.borrow_mut() = true;
        &mut self.keys
    }

    pub fn children_mut(&mut self) -> &mut Vec<i32> {
        *self.changed.borrow_mut() = true;
        &mut self.children
    }

    pub fn values_mut(&mut self) -> &mut Vec<(i32, i32)> {
        *self.changed.borrow_mut() = true;
        &mut self.values
    }

    pub fn delete_page(&mut self, next_deleted: Option<i32>) {
        self.deleted = true;
        *self.changed.borrow_mut() = true;
        self.keys = Vec::new();
        self.children = Vec::new();
        self.values = Vec::new();
        self.next_deleted_page = next_deleted;
    }

    pub fn reallocate(&mut self) {
        self.deleted = false;
        *self.changed.borrow_mut() = true;
        self.keys = Vec::new();
        self.children = Vec::new();
        self.values = Vec::new();
        self.next_deleted_page = None;
    }
    pub fn new(max_degree: usize, id: i32) -> Self {
        if id == i32::MIN {
            panic!("Cannot write page with id i32::MIN");
        }
        Self {
            id,
            deleted: false,
            next_deleted_page: None,
            values: Vec::new(),
            keys: Vec::new(),
            children: Vec::new(),
            next_leaf: None,
            max_degree,
            changed: Rc::new(RefCell::new(true)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_from_store(
        id: i32,
        deleted: bool,
        next_deleted_page: Option<i32>,
        keys: Vec<i32>,
        children: Vec<i32>,
        values: Vec<(i32, i32)>,
        next_leaf: Option<i32>,
        max_degree: usize
    ) -> Self {
        Self {
            id,
            deleted,
            next_deleted_page,
            keys,
            children,
            values,
            next_leaf,
            max_degree,
            changed: Rc::new(RefCell::new(false)),
        }

    }
}

impl NodePage {
    // pub fn depth(&self, level: u16) -> u16 {
    //     let first = self.children.first();

    //     if let Some(first) = first {
    //         first.depth(level + 1)
    //     } else {
    //         level + 1
    //     }
    // }

    pub fn min_keys(&self) -> usize {
        (self.max_degree / 2) - 1
    }

    pub fn max_keys(&self) -> usize {
        self.max_degree - 1
    }

    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }

    #[cfg(test)]
    pub (crate) fn validate(&self, pager: &NodePager, min_key: Option<i32>, max_key: Option<i32>) {
        self.check_node_invariants(pager);
        if let Some(min_key) = min_key {
            assert!(self.keys.iter().all(|k| *k >= min_key), "All Keys must be greater or equal than min_key. min_key: {}, keys:{:?}", min_key, self.keys);
        }

        if let Some(max_key) = max_key {
            assert!(self.keys.iter().all(|k| *k < max_key), "All Keys must be less than max_key. max_key: {}, keys:{:?}", max_key, self.keys);
        }

        for i in 0..self.children.len() {
            let child_min = match i {
                0 => min_key,
                _ => Some(self.keys[i - 1]),
            };

            let child_max = match i {
                i if i < self.keys.len() => Some(self.keys[i]),
                _ => max_key,
            };

            let page = pager.read_page(self.children[i]).unwrap();

            page.validate(pager, child_min, child_max);
        }
    }

    #[cfg(test)]
    fn check_node_invariants(&self, pager: &NodePager) {
        assert!(!self.keys.is_empty(), "Keys must never be empty (if not root node): {:?}", self);
        if self.is_leaf() {
            assert_eq!(self.children.len(), 0, "Children in leaf must be always empty");
            assert_eq!(self.values.len(), self.keys.len(), "Every key must have a value in a leaf");
        } else {
            assert_eq!(
                self.children.len(),
                self.keys.len() + 1, 
                "Internal node must have one more children than keys. keys: {:?}, children: {:?}", self.keys, self.children);
            assert_eq!(self.values.len(), 0, "Internal node must not have values");
            assert!(!self.children.is_empty(), "Children must not be empty if not leaf: {:?}", self);
        }

        assert!(self.max_degree > self.keys.len(), "Max degree must be greater than key len. Keys: {:?}", self.keys);

        assert!(self.keys.windows(2).all(|pair| pair[0] < pair[1]), "Keys must be sorted. Keys in this node: {:?}", self.keys);

        if !self.is_leaf() {
            // Check if the node's children have valid keys in relation to the parent key:
            for index in 0..self.children().len() {
                let key_index = if self.keys.len() > index { index } else { self.keys.len() - 1 };
                let key = &self.keys[key_index];
                let child_res = pager.read_page(self.children[index]);
                assert!(child_res.is_ok(), "Pager cannot read child of node with id: {}", self.id);
                let child = child_res.unwrap();
                if index == self.keys.len() {
                    // last one must be greater than key
                    for k in child.keys.iter() {
                        assert!(k >= key, 
                            concat!("Last reference (ref: {}) of node (id: {}, key: {}) ",
                            "must only point to values EQUAL or GREATER than the key. ",
                            "Child: (id: {}, key: {}, all keys: {:?})"), index, self.id, key, child.id, k, child.keys
                        );
                    }
                } else {
                    for k in child.keys.iter() {
                        assert!(k < key, 
                            concat!("Reference (ref: {}) of node (id: {}, key: {}) ",
                            "must only point to values LESS than the key. ",
                            "Child: (id: {}, key: {}, all keys: {:?})"), index, self.id, key, child.id, k, child.keys
                        );
                    }
                }
            }
        }
        
    }

    // returns new allocated right node and the key (K) for the parent
    // self is the left node
    pub fn split(&mut self, pager: &NodePager) -> Result<(NodePage, i32), NodeOperationError> {
        // check invariants before split
        let middle_value_index = self.keys.len() / 2;

        let mut right_keys = self.keys.split_off(middle_value_index);
        let mut right_children = Vec::new();
        let mut right_values = Vec::new();

        let promoted_key;
        
        if !self.is_leaf() {
            right_children = self.children.split_off(middle_value_index + 1);
            promoted_key = right_keys.remove(0); // Key promotes and gets removed
        } else {
            right_values = self.values.split_off(middle_value_index);
            promoted_key = right_keys[0]; // Key stays in right node and promotes
        }

        let mut right_node = pager.allocate_new_page()?;
        right_node.values = right_values;
        right_node.keys = right_keys;
        right_node.children = right_children;
        right_node.max_degree = *self.max_degree();

        if self.is_leaf() {
            let link_to_next = self.next_leaf;
            self.next_leaf = Some(*right_node.id());
            right_node.next_leaf = link_to_next;
        }

        pager.write_page(&right_node)?;
        #[cfg(test)]
        right_node.check_node_invariants(pager);

        *self.changed().borrow_mut() = true;
        pager.write_page(self)?;

        Ok((right_node, promoted_key))
    }

    fn find_key_index(&self, key: i32) -> FindKeyResponse {
        binary_search(&key, &self.keys)
    }

    #[cfg(test)]
    pub fn print_all_nodes(&self, pager: &NodePager) {
        println!("ID: {}, keys: {:?}, values: {:?}", self.id, self.keys, self.values);
        for c in self.children.iter() {
            let next = pager.read_page(*c).unwrap();
            next.print_all_nodes(pager);
        }
    }

    fn insert_key_value(&mut self, key: i32, value: (i32, i32)) -> Result<(), NodeOperationError> {
        match self.find_key_index(key) {
            FindKeyResponse::LessThan(i) => {
                self.keys.insert(i, key);
                self.values.insert(i, value);
                *self.changed.borrow_mut() = true;
                Ok(())
            },
            FindKeyResponse::GreaterThanTheLast(_) => {
                self.keys.push(key);
                self.values.push(value);
                *self.changed.borrow_mut() = true;
                Ok(())
            },
            FindKeyResponse::Equal(_) => {
                Err(NodeOperationError::TryInsertDuplicate)
            },
        }
    }
    
    pub fn insert(&mut self, pager: &NodePager, key: i32, value: (i32, i32), unique: bool) -> Result<(), NodeOperationError>{
        if !unique {
            panic!("Duplicate keys are not handled yet. Unique must always be true.");
        }
        // if is leaf, then insert key and value
        if self.is_leaf() {
            self.insert_key_value(key, value)
        } else {
            // if not leaf:

            // 1. find correct Node
            let mut child_node_index= self.keys.iter().enumerate()
                .find(|(_, k)| key < **k)
                .map(|(i, _)| i)
                .unwrap_or(self.children.len() - 1);

            // 2. if Node is full, split
            let mut child = pager.read_page(self.children[child_node_index])?;
            let mut split = false;
            if child.is_full() {
                    split = true;
                    let (rnode, promoted_key) = child.split(pager)?;

                    if self.keys.len() == child_node_index {
                        // This means: no key has been found, so append at the end
                        self.keys.push(promoted_key);
                        // self.children[child_node_index] = *lnode.id();
                        self.children.push(*rnode.id());

                        if key > promoted_key {
                            // They key that we want to insert is greater than the 
                            // new key given by the new right node
                            // so, we need to put the (key,value) into this right node
                            child_node_index += 1;
                        }
                    } else {
                        self.keys.insert(child_node_index, promoted_key);
                        self.children.insert(child_node_index + 1, *rnode.id());

                        if key >= promoted_key {
                            // if the promoted key is less thant the new key
                            // add it on the right side of the key.
                            child_node_index += 1;
                        }
                    }

                    *self.changed.borrow_mut() = true;
            }
        
            // 3. insert into next node
            if split {
                // node_index has changed, that's why the child is loaded again
                child = pager.read_page(self.children[child_node_index])?;
            }

            child.insert(pager, key, value, unique);
            pager.write_page(&child)?;
            #[cfg(test)]
            self.check_node_invariants(pager);

            Ok(())
        }
    }

    pub fn is_full(&self) -> bool {
        self.keys.len() >= self.max_keys()
    }

    pub fn can_lend_keys(&self) -> bool {
        self.keys.len() > self.min_keys()
    }

    pub fn is_less_than_minimal(&self) -> bool {
        self.keys.len() < self.min_keys()
    }

    pub fn find_left_most_node(&self, pager: &NodePager) -> Result<Option<i32>, NodeOperationError> {
        if self.is_leaf() {
            Ok(Some(*self.id()))
        } else {
            let page = pager.read_page(self.children[0])?;
            page.find_left_most_node(pager)
        }
    }

    pub fn find_node(&self, pager: &NodePager, key: i32) -> Result<Option<i32>, NodeOperationError> {
        match self.find_key_index(key) {
            // is leaf
            FindKeyResponse::GreaterThanTheLast(_) if self.is_leaf() => Ok(None),
            FindKeyResponse::LessThan(_) if self.is_leaf() => Ok(None),
            FindKeyResponse::Equal(_) if self.is_leaf() => Ok(Some(*self.id())),
            // internal node
            FindKeyResponse::GreaterThanTheLast(i) 
                | FindKeyResponse::Equal(i) => {
                    let child = pager.read_page(self.children[i + 1])?;
                    child.find_node(pager, key)
            },
            FindKeyResponse::LessThan(i) => {
                let child = pager.read_page(self.children[i])?;
                child.find_node(pager, key)
            }
        }
    }

    pub fn find_value(&self, pager: &NodePager, key: i32) -> Result<Option<(i32, i32)>, NodeOperationError> {
        if let Some(node) = self.find_node(pager, key)? {
            let page = pager.read_page(node)?;
            for (i, k) in page.keys().iter().enumerate() {
                if *k == key {
                    return Ok(Some(page.values[i]));
                }
            }
            Err(NodeOperationError::CorruptedNode)
        } else {
            Ok(None)
        }
    }

    // Delete a key from this subtree. Returns the removed value if present.
    pub fn delete(&mut self, pager: &NodePager, key: i32) -> Result<Option<(i32, i32)>, NodeOperationError> {
        if self.is_leaf() {
            match binary_search(&key, &self.keys) {
                FindKeyResponse::Equal(pos) => {
                    self.keys.remove(pos);
                    let v = self.values.remove(pos);
                    *self.changed.borrow_mut() = true;
                        return Ok(Some(v));
                },
                _ => return Ok(None),
            }
        }

        let node_index = self.keys.iter().enumerate()
            .find(|(_, k)| key < **k)
            .map(|(i, _)| i)
            .unwrap_or(self.children.len() - 1);

        let mut target_node = pager.read_page(self.children[node_index])?;
        // Refactoring: MERGE
        // self.merge(node_index)
        if target_node.is_less_than_minimal() {
            
            // Is there a left node from target node?
            let left_neighbor_can_lend = if node_index > 0  {
                let left = pager.read_page(self.children[node_index - 1])?;
                let can_lend = left.can_lend_keys();
                Some((left, can_lend))
            } else {
                None
            };

            // Is there a right node from target node?
            let right_neighbor_can_lend = if node_index + 1 < self.children.len() {
                let right = pager.read_page(self.children[node_index + 1])?;
                let can_lend = right.can_lend_keys();
                Some((right, can_lend))
            } else {
                None
            };

            if let Some((mut left_node, true)) = left_neighbor_can_lend {
                // There is a left_node and the left node can lend
                if target_node.is_leaf() {
                    let k = left_node.keys.pop().ok_or(NodeOperationError::CorruptedNode)?;
                    let v = left_node.values.pop().ok_or(NodeOperationError::CorruptedNode)?;
                    target_node.keys.insert(0, k);
                    target_node.values.insert(0, v);
                    self.keys[node_index - 1] = target_node.keys[0];
                } else {
                    let left_key = left_node.keys.pop().ok_or(NodeOperationError::CorruptedNode)?;
                    let left_child = left_node.children.pop().ok_or(NodeOperationError::CorruptedNode)?;
                    let parent_key = self.keys[node_index - 1];
                    target_node.keys.insert(0, parent_key);
                    target_node.children.insert(0, left_child);
                    self.keys[node_index - 1] = left_key;
                }
                *target_node.changed.borrow_mut() = true;
                *left_node.changed.borrow_mut() = true;
                *self.changed.borrow_mut() = true;

                #[cfg(test)]
                {
                    target_node.check_node_invariants(pager);
                    left_node.check_node_invariants(pager);
                }

                pager.write_page(&target_node)?;
                pager.write_page(&left_node)?;
            } else if let Some((mut right_node, true)) = right_neighbor_can_lend {
                // There is a right_node and the right node can lend
                if target_node.is_leaf() {
                    let k = right_node.keys.remove(0);
                    let v = right_node.values.remove(0);
                    target_node.keys.push(k);
                    target_node.values.push(v);
                    self.keys[node_index] = right_node.keys[0];
                } else {
                    let right_key = right_node.keys.remove(0);
                    let right_child = right_node.children.remove(0);
                    let parent_key = self.keys[node_index];
                    target_node.keys.push(parent_key);
                    target_node.children.push(right_child);
                    self.keys[node_index] = right_key;
                }

                *target_node.changed.borrow_mut() = true;
                *right_node.changed.borrow_mut() = true;
                *self.changed.borrow_mut() = true;

                #[cfg(test)]
                {
                    target_node.check_node_invariants(pager);
                    right_node.check_node_invariants(pager);
                }

                pager.write_page(&target_node)?;
                pager.write_page(&right_node)?;
            } else {
                // must merge with a sibling:
                if let Some((mut left_node, _)) = left_neighbor_can_lend {
                    // Target node will be deleted and all keys, children, values will be moved to the left node
                    // the left node will then be the new target node
                    let left_index = node_index - 1;
                    // Remove reference to the target node
                    self.children.remove(node_index);

                    // remove left key from parent
                    let separator = self.keys.remove(left_index);
                    if left_node.is_leaf() {
                        left_node.keys.extend(std::mem::take(&mut target_node.keys));
                        left_node.values.extend(std::mem::take(&mut target_node.values));
                        // Get next leaf from deleted target node
                        left_node.next_leaf = target_node.next_leaf;
                    } else {
                        left_node.keys.push(separator);
                        left_node.keys.extend(std::mem::take(&mut target_node.keys));
                        left_node.children.extend(std::mem::take(&mut target_node.children));
                    }
                    *left_node.changed.borrow_mut() = true;
                    *self.changed.borrow_mut() = true;
                    #[cfg(test)]
                    {
                        left_node.check_node_invariants(pager);
                    }
                    pager.write_page(&left_node)?;
                    pager.delete_page(*target_node.id())?;

                    // delete must be executed in the left node
                    target_node = left_node;
                } else if let Some((mut right_node, _)) = right_neighbor_can_lend {
                    // merge target node with the right node and delete the right node completely
                    self.children.remove(node_index + 1);

                    let separator = self.keys.remove(node_index);
                    if target_node.is_leaf() {
                        target_node.keys.extend(std::mem::take(&mut right_node.keys));
                        target_node.values.extend(std::mem::take(&mut right_node.values));
                        target_node.next_leaf = right_node.next_leaf;
                    } else {
                        // set parents separator in target_node to match the references to the children
                        target_node.keys.push(separator);
                        target_node.keys.extend(std::mem::take(&mut right_node.keys));
                        target_node.children.extend(std::mem::take(&mut right_node.children));
                    }

                    *target_node.changed.borrow_mut() = true;
                    *self.changed.borrow_mut() = true;

                    #[cfg(test)]
                    {
                        target_node.check_node_invariants(pager);
                    }
                    pager.write_page(&target_node)?;
                    pager.delete_page(*right_node.id())?;
                }
                // No need for else-statement, because there should never be another state. Either left or right node must exist.
            }
        }

        let res = target_node.delete(pager, key);
        pager.write_page(&target_node)?;

        res
    }
}

#[cfg(test)]
mod tests {
    use crate::tree::node::{FindKeyResponse, binary_search};

    #[test]
    fn should_find_index_for_value_smaller_than_index_7() {
        let keys = vec![1, 2, 3, 4, 5, 6, 60, 70, 80, 90, 91, 92, 93, 94, 95, 105, 110, 200, 500];
        let index = binary_search(&68, &keys);
        assert!(matches!(index, FindKeyResponse::LessThan(7)), "Should be LessThan(7) but is {:?}", index);
    }

    #[test]
    fn should_find_index_for_value_smaller_than_index_18() {
        let keys = vec![1, 2, 3, 4, 20, 50, 60, 70, 80, 90, 91, 92, 93, 94, 95, 105, 110, 200, 500];
        let index = binary_search(&400, &keys);
        assert!(matches!(index, FindKeyResponse::LessThan(18)), "Should be LessThan(18) but is {:?}", index);
    }

    #[test]
    fn should_find_last_index_greater_than_last() {
        let keys = vec![1, 2, 3, 4, 20, 50, 60, 70, 80, 90, 91, 92, 93, 94, 95, 105, 110, 200, 500];
        let index = binary_search(&600, &keys);
        assert!(matches!(index, FindKeyResponse::GreaterThanTheLast(18)), "Should be GreaterThanTheLast(18) but is {:?}", index);
    }

    #[test]
    fn should_find_value_that_is_equal() {
        let keys = vec![1, 2, 3, 4, 20, 50, 60, 70, 80, 90, 91, 92, 93, 94, 95, 105, 110, 200, 500];
        let index = binary_search(&1, &keys);
        assert!(matches!(index, FindKeyResponse::Equal(0)), "Should be Equal(0) but is {:?}", index);
    }

}
