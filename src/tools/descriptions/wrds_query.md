Run a read-only SQL query against WRDS (Wharton Research Data Services). WRDS is a PostgreSQL data platform for finance, accounting, and economics research (CRSP, Compustat, IBES, TAQ, etc.); there is no REST API, so this tool queries the database directly over an SSL connection.

Reference a dataset as `library.table`, where a WRDS "library" is a Postgres schema — e.g. `crsp.dsf` (CRSP daily stock file), `comp.funda` (Compustat fundamentals annual), `ff.factors_daily` (Fama-French factors). Use `wrds_list_libraries`, `wrds_list_tables`, and `wrds_describe_table` to discover what is available and the exact column names before querying.

The query must be a single read-only `SELECT` (the session is forced read-only; writes/DDL are rejected). WRDS tables are very large, so ALWAYS constrain with a `WHERE` clause and keep `rowLimit` small. Use `jq` (JMESPath) to project only the columns you need and cut token cost.

Example: `SELECT permno, date, ret, prc, vol FROM crsp.dsf WHERE permno = 14593 AND date >= '2023-01-01' ORDER BY date`.
