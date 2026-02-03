Architecture
---

TouchHouse is a fast, columnar OLAP database, designed to process millions of rows of data in milliseconds.

Simple by design:
* Focused on analytical workloads, not OLTP.
* Single binary, no external coordination.
* Immutability - background merges, instead of in-place updates.

___
## Modules
- `src/main.rs` - Server entry point and connection handling
- `src/engines/` - Database engines implementations
- `src/sql/` - Sql parsing and execution
- `src/storage/` - Storage specific implementations
- `src/config.rs` - Configuration management with environment variables


---
## Performance

Read in `perf_results.txt`.

---
## Column-oriented storage

TouchHouse stores each column in separate file. This allows to:
* Open/Read only required files.
* Better compression rate due to homogeneous data.

Each column has its own best suited compression algorithm, name, type, constraints and data.

On its own, column data consists of granules. 

Granule is an array of values, with length of 8192 (at the time writing, [sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs) does not support `SETTINGS` in `CREATE TABLE` command, which forces all table to have this index of granularity) values. Each granule is **_separate_**, which allows to read/decompress/deserialize only required granules with values to speedup queries. 

TouchHouse uses [rkyv](https://rkyv.org/) library for serialization/deserialization, which supports zero copy deserialization (allows not to allocate space for values which will not appear in the end).

Currently, for all value types, TouchHouse uses `LZ4HC` (with level = 3), as it allows good compression and fastest decompression speeds. However, it's easy to add other compression algorithms (look `src/storage/compression.rs`). Unfortunately, at the time writing, [sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs) does not support `CODEC` param to define compression in create table, thus forcing `LZ4HC` for all columns.

---
## Table parts

Database tries to stay as immutable as possible to remove possibility of having database in incomplete way. Which is why each `INSERT` does not modify any data, but creates new folder -table part.

Table part contains: 
* `part.inf` - information (name, created_at, row_count, columns in part).
* `col_name1.bin`, `col_name2.bin`, ... - columns stored each in a separate file.

Table part name is UuidV7 when part was created.

---
## Table engines

Each table has engine. Engine defines how your data is ordered and stored on disk.

Available engines:
* `MergeTree` - standard engine. sorts rows in `ORDER BY` order. does not modify them.
* `ReplacingMergeTree` - engine for editing rows. When it finds rows with the same PK values, it replaces with the newest row values.

`ReplacingMergeTree` example:
``` 
PK indexes: [0, 1]  
 
Row0: [1, 2, 3, 4] <- same PK values (1, 2)  
Row1: [4, 2, 3, 4]  
Row2: [1, 2, 33, 42] <- same PK values (1, 2)  
 
Replaces row [1, 2, 3, 4] with new row: [1, 2, 33, 42]  
 
Returns:  
Row0: [1, 2, 33, 42]  
Row1: [4, 2, 3, 4]  
```

---
## Background merges

To speedup `SELECT` queries and use less storage, TouchHouse merge table parts is background. When system is not busy with queries (`background_merge_available_under` param in `touch_config.toml`), database locks tables and merges two parts using table engine specified in table settings.

---
## SQL support

Allowed value types:
* Null
* String
* Uuid
* Bool
* Int8
* Int16
* Int32
* Int64
* UInt8
* UInt16
* UInt32
* UInt64

TouchHouse supported commands:
* `CREATE DATABASE [IF NOT EXISTS] db.table_name`.
* `CREATE TABLE [IF NOT EXISTS] db.table_name (name1 [type1] [NULL|NOT NULL] [DEFAULT val1], name2 [type2] [NULL|NOT NULL] [DEFAULT val2], ...) [ENGINE = engine] [PRIMARY KEY expr_list] [ORDER BY expr_list]`.
* `SELECT expr_list FROM db.table_name WHERE expr ORDER BY expr_list LIMIT uint_val OFFSET uint_val`.
* `INSERT INTO db.table_name (name1, name2, ...) VALUES (val1, val2, ...), (val1a, val2, ...)`,
* `DROP TABLE [IF NOT EXISTS] db.table_name`.
* `DROP DATABASE [IF NOT EXISTS] db`.
* `exit`.
* Nested `SELECT`.

---
## Query pipeline

1. SQL to AST conversion using [sqlparser-rs](https://github.com/apache/datafusion-sqlparser-rs).
2. AST to Logical plan - data validation, conversion to `LogicalPlan` struct (`src/sql/logical_plan`).
3. Plan optimization (`src/sql/plan_optimizaton`).
4. Logical plan to Physical plan - simplification of logical plan (`src/sql/sql_parser.rs`).
5. Physical plan parallel execution (`src/sql/execution`). returns either `Error` or `OutputTable`.

---
## Unsafe code

TouchHouse uses only 2 unsafe functions:
* `memmap2::Mmap::map` - mmap column granules for read. However, scan locks table for read-only access, thus not allowing other threads of database to modify column files. _Undefined Behavior_ could only happen if user of the system, where TouchHouse runs, tries to modify column file. But this is unlikely, as all data is stored in `rkyv` deserialized format, which is not human friendly.
* `rkyv::access_unchecked` - to [access](https://rkyv.org/architecture/archive.html) granule data. Benchmarks showed, that unchecked version is 2.4 million times faster - 509μs (checked) vs 210ps (unchecked). To mitigate corruption issues, each column has 32-bit CRC checksum, which is checked right after opening file.

---
## System configuration

* `storage_directory` - storage directory. DEFAULT "db_files/".
* `tcp_socket` - TCP socket to accept connections. DEFAULT "127.0.0.1:7070".
* `max_connections` - max connection at a time. DEFAULT 100.
* `log_level` - database logging. DEFAULT 1. Allowed values:
	- 1 => Info
	- 2 => Warn
	- 3 => Error
* `background_merge_available_under` - Signifies when database can do background merges of parts, depending on database load. DEFAULT 5.

---
## Resource utilization:
* Vectorized select with compiled filter and low allocation amount.
* Zero-copy access/deserialization granule access, sequential reads.

---
## Comparison

vs Clickhouse:
* 30% slower.
* Lighter executable - TouchHouse 5.9MB vs ClickHouse 728.5MB.

vs MariDB:
* 38x faster.
* Requires complex installation. TouchHouse is a single binary.

---
## Known Limitations
* No support for JOIN operations.
* No support for AGGREGATE operations.
* Single-node only.
* Limited to 8192-row granules.
* No user authentication.
