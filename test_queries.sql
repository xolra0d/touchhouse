CREATE DATABASE default
-------------------
CREATE TABLE default.users (id UInt64, name String, age UInt8, email String) ENGINE = MergeTree() PRIMARY KEY (id, age) ORDER BY (id, age, name)
INSERT INTO default.users (id, name, age, email) VALUES (1, 'Alice', 25, 'alice@example.com'), (2, 'Alice', 30, 'alice@example.com')
INSERT INTO default.users (id, name, age, email) VALUES (3, 'Charlie', 35, 'charlie@example.com'), (4, 'Diana', 28, 'diana@example.com')
SELECT name, age FROM default.users
SELECT name, age FROM default.users WHERE id > 2 AND id <= 4 LIMIT 1
-----
CREATE TABLE default.user_versions (id UInt64, version String, name String, updated_at UInt64) ENGINE = ReplacingMergeTree() PRIMARY KEY (id) ORDER BY (id, updated_at)
INSERT INTO default.user_versions (id, version, name, updated_at) VALUES (1, 'v1', 'Alice', 1000), (2, 'v1', 'Bob', 1000), (3, 'v1', 'Charlie', 1000)
INSERT INTO default.user_versions (id, version, name, updated_at) VALUES (1, 'v2', 'Alice Updated', 2000), (2, 'v2', 'Bob Updated', 2000)
INSERT INTO default.user_versions (id, version, name, updated_at) VALUES (1, 'v3', 'Alice Final', 3000)
SELECT * FROM default.user_versions
