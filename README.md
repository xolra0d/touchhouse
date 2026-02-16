# TouchHouse - Blazingly Fast Column-Oriented Database
___
## Installation & Usage

### Quick Start
1. **Grap binary**:
   ```bash
   curl -L https://github.com/xolra0d/touchhouse/releases/latest/download/touchhouse -o touchhouse
   ```

2. **Run the server**:
   ```bash
   chmod +x touchouse
   ./touchouse
   ```
   The server will create default configuration and start on `127.0.0.1:7070`.

### Example Usage

### Client Connection

```bash
python3 client.py HOST PORT
```

### Example Database Operations
```bash
CREATE DATABASE mydb;
CREATE TABLE my_db.users (id UUID, name String, age UInt8) ENGINE=MergeTree ORDER BY (name, age)
INSERT INTO my_db.users (id, name, age) VALUES ('123e4567-e89b-12d3-a456-426614174000', 'Alice', 30)
SELECT * FROM my_db.users WHERE name = 'Alice' LIMIT 1
```

## Docs
Read more in `docs/`.

For more in-depth description `cargo doc --open`.

## Perfomance

In last version (2.1.0) aggregation support was added. As of now, I could not find a good solution how to parallelize query execution. If perfomance is necessary, it's recommended to switch to 2.0.0, where queries execution was 10x times faster.

I suspect false sharing to cause a massive slowdown (6x slow). Inverstagion is going.. 

## License
This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
