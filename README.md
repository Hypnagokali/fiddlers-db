# playdb

A small row-based toy database written in Rust.

This project exists to learn core RDBMS internals by building them directly: pages, heap storage, free space management, system catalogs, and a simple unique index.

It is intentionally incomplete and not ACID-compliant. The focus is educational clarity over production correctness.

## Current Capabilities

- Create a database directory and initialize system tables.
- Create user tables with a typed schema (`Int`, `Varchar(n)`, `Byte`).
- Insert, read, update, and delete rows.
- Sequential scans and predicate filtering.
- Optional unique index support via a B+ tree (single-column `Int` keys).
- Basic sequence support for auto-increment-like integer columns.
- Hash join support at query-result level.

## Important Limitations

- No transactions (no commit/rollback, no isolation levels).
- No constraints framework (no PK/FK/check/not-null handling).
- No WAL/recovery/crash safety.
- No SQL parser or query planner (operations are API-driven).
- B+ tree only supports unique indexes today.
- No duplicated index keys, no composite keys, no non-`Int` indexed key types.
- Minimal error handling in some internals (`unwrap`/`panic` paths still exist).

## Module Overview

### `data`
Row/page representation and serialization.

What it does:
- Defines page layout and metadata (`PageDataLayout`, `PageFileMetadata`).
- Implements slotted page mechanics (record slots, insert/read/update/delete markers).
- Serializes/deserializes row cells and records.

What is missing / rough edges:
- Some deserialization and integrity checks still rely on `unwrap`.
- Tuple/page implementation for heap is separate from B+ tree page format.

### `database`
Top-level orchestration and public API.

What it does:
- Creates/opens a database and initializes catalog tables (`tables`, `columns`, `sequences`, `indexes`).
- Creates tables from schema commands.
- Provides access wrappers:
  - `TableAccess` for CRUD/query operations.
  - `SeqAccess` for sequence values.
- Wires table metadata to index metadata.

What is missing / rough edges:
- No SQL layer; API calls are manual.
- No transactional semantics.
- Catalog and access-layer wiring is intentionally simple and still tightly coupled in places.

### `store`
Physical storage abstraction and file-backed implementation.

What it does:
- Defines the `Store` trait for page allocation/read/write/iteration.
- Implements `FileStore` with one file per table/FSM structure.
- Handles page-file metadata and page iteration.
- Exposes B+ tree loading through the store boundary.

What is missing / rough edges:
- `delete_all` is currently non-destructive (prints files instead of removing them).
- Some operations can leave inconsistent state on partial failures (no atomic multi-step writes).
- `Store` still mixes concerns that could be split further (raw I/O vs higher-level structure lifecycle).

### `table`
Schema and row model.

What it does:
- Defines table schema (`TableSchema`, `Column`, `ColumnType`).
- Defines row/cell value types and validation.
- Validates type and varchar length on insert/update paths.

What is missing / rough edges:
- No formal null support (the project uses simplifications/sentinels in some places).
- No general constraint enforcement layer.

### `tree`
B+ tree index implementation and its own storage layer.

What it does:
- Implements B+ tree nodes and on-disk node paging.
- Supports unique index lookup/insert/delete for indexed table access.
- Stores `(page_id, slot_id)` references as index payload.

What is missing / rough edges:
- Separate storage implementation from heap `store` (known architectural duplication).
- Only unique indexes supported.
- No duplicate keys, no composite keys, no generalized index type system.

### `fsm`
Free Space Map implementation.

What it does:
- Tracks available free space categories per heap page.
- Helps find a page for insert without full table scan.
- Implemented as a tree structure inspired by PostgreSQL’s FSM approach.

What is missing / rough edges:
- Robustness features are limited; corruption handling/self-healing is minimal.
- Several code paths still use `panic` on invariant violations.

## Learning Focus

This codebase is meant to be read, modified, and broken on purpose while learning.
If you want to experiment, good next extensions are:

- duplicate-key/non-unique indexes
- basic transaction log (WAL)
- not-null + unique constraint checks
- shared storage abstraction for heap + B+ tree pages
