-- Record the search_path scope a query ran under, so re-opening it from
-- history reconstructs the same scoped tab.
--
-- NULL = database-level query (no `SET LOCAL search_path`); a non-null value
-- is the single schema the originating tab was scoped to.  Pre-existing rows
-- are left NULL, which correctly reads as "unscoped".
ALTER TABLE query_history ADD COLUMN schema TEXT;
