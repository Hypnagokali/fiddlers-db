use std::time::{Duration, Instant};

use rand::Rng;
use crate::{database::Database, store::file_store::FileStore, table::{ColumnType, table::Cell, table::Row}};

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

#[allow(unused)]
fn create_table_persons(db: &Database<FileStore>) {
    let persons_table = db.create_table("persons", vec![
        ("id", ColumnType::Int, true, true),
        ("name", ColumnType::Varchar(100), false, false),
        ("number", ColumnType::Int, false, false),
        ("flag", ColumnType::Byte, false, false),
    ]).unwrap();

    let person_acc = db.table_access(persons_table.clone()).unwrap();
    let mut seq_acc = db.seq_access_for_table(persons_table).unwrap();

    for i in 1..1000_000 {
        if i % 10000 == 0 {
            println!("Added {} entries", i);
        }
        let next_id = seq_acc.next_val("id").unwrap();
        
        person_acc.insert(&Row::new(
            vec![Cell::Int(next_id), Cell::Varchar("Some".to_owned()), Cell::Int(120 + i), Cell::Byte(1)]
        )).unwrap();
    }
}

#[derive(Debug, Clone, Copy)]
struct BenchmarkResult {
    hits: usize,
    misses: usize,
    elapsed: Duration,
}

fn generate_query_ids(repetitions: usize) -> Vec<i32> {
    let mut rng = rand::thread_rng();
    (0..repetitions)
        .map(|_| rng.gen_range(1..1_000_000))
        .collect()
}

fn benchmark_queries<F>(query_values: &[i32], mut query: F) -> BenchmarkResult
where
    F: FnMut(i32) -> usize,
{
    let start = Instant::now();
    let mut hits = 0usize;
    let mut misses = 0usize;
    let print_every = query_values.len() / 10;

    for (i, value) in query_values.iter().enumerate() {
        if i % print_every == 0 {
            println!("Queries: {}", i);
        }
        let match_count = query(*value);
        if match_count == 0 {
            misses += 1;
        } else {
            hits += match_count;
        }
    }

    BenchmarkResult {
        hits,
        misses,
        elapsed: start.elapsed(),
    }
}

fn average_result(results: &[BenchmarkResult]) -> BenchmarkResult {
    let total_hits: usize = results.iter().map(|r| r.hits).sum();
    let total_misses: usize = results.iter().map(|r| r.misses).sum();
    let total_millis: u128 = results.iter().map(|r| r.elapsed.as_millis()).sum();
    let count = results.len() as u128;

    BenchmarkResult {
        hits: total_hits / results.len(),
        misses: total_misses / results.len(),
        elapsed: Duration::from_millis((total_millis / count) as u64),
    }
}

fn print_benchmark(label: &str, repetitions: usize, result: &BenchmarkResult) {
    println!(
        "  {}: {:?} total, {:.3} ms/query, hits={}, misses={}",
        label,
        result.elapsed,
        result.elapsed.as_millis() as f64 / repetitions as f64,
        result.hits,
        result.misses,
    );
}

fn main() {
    let db = Database::new("testdb");
    // create_table_persons(&db);

    let repetitions = 100;
    let runs = 5;
    let table = db.read_table("persons").unwrap();
    let tbl_acc = db.table_access(table).unwrap();

    let mut results = Vec::with_capacity(runs);

    println!("Benchmark started:");
    println!("  repetitions per run: {}", repetitions);
    println!("  runs: {}", runs);

    for run in 1..=runs {
        let query_ids = generate_query_ids(repetitions);

        // Indexed queries
        let result = benchmark_queries(&query_ids, |id| {
            tbl_acc.find("id", Cell::Int(id)).unwrap().rows().len()
        });

        // Sequential scan queries
        // let result: BenchmarkResult = benchmark_queries(&query_ids, |id| {
        //     tbl_acc.find("number", Cell::Int(120 + id)).unwrap().rows().len()
        // });

        println!("Run {}:", run);
        print_benchmark("indexed lookup (id)", repetitions, &result);
        // print_benchmark("non-indexed lookup (number)", repetitions, &non_indexed);

        results.push(result);
        // non_indexed_results.push(non_indexed);
    }

    let avg = average_result(&results);

    println!("Average over {} runs:", runs);
    print_benchmark("query result", repetitions, &avg);
}
