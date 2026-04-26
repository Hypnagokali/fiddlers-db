use crate::{database::Database, store::file_store::FileStore, table::{ColumnType, table::{Cell, Row}}};

// ignore dead_code while developing
#[allow(dead_code)]
mod table;
#[allow(dead_code)]
mod data;
#[allow(dead_code)]
mod store;
#[allow(dead_code)]
mod database;
#[allow(unused)]
mod tree;

#[allow(unused)]
mod fsm;

fn create_table_persons(db: &Database<FileStore>) {
    let persons_table = db.create_table("persons", vec![
        ("id", ColumnType::Int, true, true),
        ("name", ColumnType::Varchar(100), false, false),
        ("number", ColumnType::Int, false, false),
        ("flag", ColumnType::Byte, false, false),
    ]).unwrap();

    let person_acc = db.table_access(persons_table.clone()).unwrap();
    let mut seq_acc = db.seq_access_for_table(persons_table).unwrap();

    // As soon as FSM is implemented, an example with a million rows should be possible
    for i in 1..20000 {
        if i % 1000 == 0 {
            println!("Added {} entries", i);
        }
        let next_id = seq_acc.next_val("id").unwrap();
        
        person_acc.insert(&Row::new(
            vec![Cell::Int(next_id), Cell::Varchar("Some".to_owned()), Cell::Int(120 + i), Cell::Byte(1)]
        )).unwrap();
    }
}

fn find_by_id_index(db: &Database<FileStore>, id: i32) {
    let table = db.read_table("persons").unwrap();
    let tbl_acc = db.table_access(table).unwrap();

    for (_, r) in  tbl_acc.find("id", Cell::Int(id)).unwrap().rows() {
        println!("{:?}", r);
    }
}

fn find_by_number_without_index(db: &Database<FileStore>, num: i32) {
    let table = db.read_table("persons").unwrap();
    let tbl_acc = db.table_access(table).unwrap();

    for (_, r) in  tbl_acc.find("number", Cell::Int(num)).unwrap().rows() {
        println!("{:?}", r);
    }
}


fn main() {
    let db = Database::new("testdb");
    // create_table_persons(&db);
    // find_by_id_index(&db, 19999);
    // find_by_number_without_index(&db, 20000);
}
