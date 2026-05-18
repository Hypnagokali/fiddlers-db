# Fiddlers-DB

A small row-based single threaded toy database written in Rust - to learn the main concepts of databases and to share this experience.

## The Name
From fiddle around and rs for rust 🙃.

## Introduction

I created this project as a Christmas project to deeply understand (to feel) the core concepts of a row base database.

Originally, I just wanted to implement a small one-table-database that is able use an index. But then it got a bit out of hand and I realized that it would be cool to have at least the basic CRUD operations. Later, it turned out that I really needed an efficient way of finding a page with enough space when I want to insert many rows. And suddenly the small Christmas project took more than 4 months.

## Disclaimer

This project exists to learn core RDBMS internals by building them directly: pages, heap storage, free space management, system catalogs, and a simple unique index.

It is intentionally incomplete and not ACID-compliant. The focus is educational clarity over production correctness.

## Goals

- [x] Learn about page layout and physical storage
- [x] Learn how an index basically works and get a feeling for why it's so much faster
- [x] Learn how deletion and updates may work
- [x] Understand fragmentation and how to solve that (*)
- [x] Learn how to efficiently insert new rows

*I learned about how to solve fragmentation, but a solution like VACUUM (FULL) is not implemented yet, so the database will become fragmented at some point in time and it will never shrink.

## Current Capabilities

- Create a database directory and initialize system tables.
- Create user tables with a typed schema (`Int`, `Varchar(n)`, `Byte`).
- Insert, read, update, and delete rows.
- Sequential scans and predicate filtering.
- Optional unique index support via a B+ tree (single-column `Int` keys).
- Basic sequence support for auto-increment-like integer columns.
- Hash join support at query-result level.

## Important Limitations

- Single threaded
- No transactions (no commit/rollback, no isolation levels).
- No constraints framework (no PK/FK/check/not-null handling).
- No NULL values
- No WAL/recovery/crash safety.
- No SQL parser or query planner (operations are API-driven).
- No compaction 
- B+ tree only supports unique indexes with a single key today.
- No duplicated index keys, no composite keys, no non-`Int` indexed key types.
- No Server to interact with.

## Module Overview

### `database`
Top-level orchestration and public API.

What it does:
- Creates/opens a database and initializes catalog tables (`tables`, `columns`, `sequences`, `indexes`).
- Can create tables with indexed and sequenced columns.
- Provides access wrappers:
  - `TableAccess` for CRUD/query operations.
  - `SeqAccess` for sequence values.

What is missing / rough edges:
- No SQL layer; API calls are manual.
- No transactional semantics.

### `data`
Page that holds raw data. Page Serialization and deserialization.

What it does:
- Defines page layout and metadata (`PageDataLayout`, `PageFileMetadata`).
- Implements slotted page mechanics (record slots, insert/read/update/delete markers).
- Serializes/deserializes row cells and records.

What is missing / rough edges:
- Tuple/page implementation for heap is separate from B+ tree page format.
- Raw pointers into the byte array (PageHeader, PageData) would be more efficient instead of serialization and deserialization of the struct.

What is a slot?
- In PostgreSQL this is called the line pointer, it points to a record (offset and length) and can be marked as deleted.

Inserting into deleted slots
- Currently, it is possible to insert directly into deleted slots, that leads to additional slot allocation if the data doesn't fit perfectly
- This is not perfect.

### `store`
Physical storage abstraction and file-backed implementation.

What it does:
- Defines the `Store` trait for page allocation/read/write/iteration.
- Implements `FileStore` with one file per table/FSM structure.
- Handles page-file metadata and page iteration.
- Exposes B+ tree loading through the store boundary.

What is missing / rough edges:
- Operations can leave inconsistent state on partial failures.
- `Store` still mixes concerns that could be split further (raw I/O vs higher-level structure lifecycle).
- `FileStore` directly writes to the filesystem instead of using a buffer management.

### `table`
Schema and row model.

What it does:
- Defines table schema (`TableSchema`, `Column`, `ColumnType`).
- Defines row/cell value types and validation.
- Validates type and VARCHAR length on insert/update paths.

What is missing / rough edges:
- No NULL support.
- Expensive clone operation on TableSchema and Column
- Expensive deserialization/serialization of `Row` instead of pointers / references into the page data (a buffer for referenced pages would be needed to hold the data long enough in memory)

### `tree`
B+ tree index implementation and its own storage layer.

What it does:
- Implements B+ tree nodes and on-disk node paging.
- Supports unique index lookup/insert/delete for indexed table access.
- Stores `(page_id, slot_id)` references as index payload.

What is missing / rough edges:
- Separate storage implementation from heap `store` (known architectural duplication).
- Has a custom page implementation (it would be better to use `Page` for storing the nodes)
- Only unique indexes supported.
- No duplicate keys, no composite keys, no generalized index type system.

Why are only int keys supported:
Because it's just about learning the basic concepts, I decided to implement a simple B+ Tree with one fixed key type and a payload.
To index values of different kinds and different lengths, the algorithm can no longer make a decision based on the number of keys whether the node should be split or merged, it must decide it based on the size.

### `fsm`
Free Space Map implementation.

What it does:
- Tracks available free space categories per heap page.
- Helps find a page for insert without full table scan.
- Implemented as a tree structure inspired by PostgreSQL’s FSM approach.

What is missing / rough edges:
- Corruption handling/self-healing is not implemented.

## What could be done next

- duplicate-key/non-unique indexes
- Index for arbitrary types and maybe composite keys
- WAL for crash recovery, atomicity and durability.
- Transactions and isolation
- shared storage abstraction for heap + B+ tree pages
- NULL values by using a bitmap (tuple header is needed for this)

