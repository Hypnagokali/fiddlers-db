mod node;
pub mod store;

// Refactoring:
// at least a BTree trait is needed, so that the BTree is independent of the physical storage and implementation.
// The store::Store trait could then provide an associative type for the BTree output.
// The FileStore would then return the FileBTreeStore.

pub fn read_i32_with_null(raw_value: i32) -> Option<i32> {
    if raw_value == i32::MIN {
        None
    } else {
        Some(raw_value)
    }
}

pub fn get_i32_be_bytes_from_option(value: &Option<i32>) -> Vec<u8> {
    value.unwrap_or(i32::MIN).to_be_bytes().to_vec()
}
