use std::{fmt::Display, ops::Deref, rc::Rc};

pub mod table;

#[derive(Debug, PartialEq, Clone)]
pub enum ColumnType {
    // Byte type
    Int,          // 0x00
    Varchar(u16), // 0x01 length is stored separately
    Byte,         // 0x02
}
#[derive(Debug)]
pub struct ColumnInner {
    pub(crate) id: i32,
    pub(crate) name: String,
    pub(crate) col_type: ColumnType,
}

#[derive(Debug, Clone)]
pub struct Column {
    inner: Rc<ColumnInner>,
}

impl Deref for Column {
    type Target = ColumnInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl PartialEq for Column {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.name == other.name && self.col_type == other.col_type
    }
}

#[derive(Debug)]
pub struct TableSchemaInner {
    pub(crate) columns: Vec<Column>,
}

// needs Clone for now, because it is shared across QueryResult and this is the quickest solution
#[derive(Debug, Clone)]
pub struct TableSchema {
    inner: Rc<TableSchemaInner>,
}

impl Deref for TableSchema {
    type Target = TableSchemaInner;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl PartialEq for TableSchema {
    fn eq(&self, other: &Self) -> bool {
        self.columns == other.columns
    }
}

impl TableSchema {
    pub fn new(columns: Vec<Column>) -> Self {
        if columns.is_empty() {
            panic!("Cannot crate empty schema");
        }
        Self {
            inner: Rc::new(TableSchemaInner { columns }),
        }
    }

    pub fn find_index_by_id(&self, column_id: &i32) -> Option<usize> {
        self.columns.iter().position(|c| &c.id == column_id)
    }

    pub fn find_index_by_name(&self, column_name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == column_name)
    }
}

impl Display for ColumnType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ColumnType::Int => f.write_str("Int"),
            ColumnType::Varchar(_) => f.write_str("Varchar"),
            ColumnType::Byte => f.write_str("Byte"),
        }
    }
}

impl ColumnType {
    pub fn is_var_size(&self) -> bool {
        match self {
            ColumnType::Int => false,
            ColumnType::Varchar(_) => true,
            ColumnType::Byte => false,
        }
    }

    pub fn raw_type(&self) -> ColumnType {
        match self {
            ColumnType::Int => ColumnType::Int,
            ColumnType::Varchar(_) => ColumnType::Varchar(0),
            ColumnType::Byte => ColumnType::Byte,
        }
    }
}

impl Column {
    pub fn new(id: i32, name: &str, col_type: ColumnType) -> Self {
        Self {
            inner: Rc::new(ColumnInner {
                id,
                name: name.to_string(),
                col_type,
            }),
        }
    }
}
