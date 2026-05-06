//! Query executor — translates parsed SQL statements into B+ tree operations.
//!
//! DDL (CREATE/DROP) operates on the catalog. DML (INSERT/SELECT/UPDATE/DELETE)
//! looks up table B+ trees via the catalog and uses indexes where applicable.

use std::{cmp::Ordering, fs, path::Path};

use anyhow::{Result, bail};

use crate::{
    sql::{Assignment, BoolExpr, ColumnDef, Columns, DataType, Operator, Statement, Value},
    storage::BPlusTree,
};

/// Result of executing a SQL statement.
pub(crate) enum QueryResult {
    /// DDL operation completed successfully and INSERT operation was performed.
    Success,
    /// SELECT returned one or more rows.
    Rows(Vec<Vec<Value>>),
    /// UPDATE / DELETE affected `n` rows.
    RowsAffected(usize),
}

/// Query execution engine backed by a catalog B+ tree.
///
/// The catalog maps prefixed keys (`table:name`, `index:name`) to root page IDs
/// and metadata. DML operations look up the catalog to obtain per-table B+ trees.
pub(crate) struct Engine {
    /// Path to the data directory.
    data_dir: String,
    /// Catalog B+ tree, `None` when no database is selected.
    catalog: Option<BPlusTree>,
    /// Currently selected database name.
    current_db: Option<String>,
}

impl Engine {
    /// Open a engine with no database selected.
    ///
    /// # Arguments
    ///
    /// * `data_dir` — path to the data directory.
    ///
    /// # Returns
    ///
    /// * `Result<Self>` — the opened engine.
    pub(crate) fn open(data_dir: &str) -> Result<Self> {
        fs::create_dir_all(data_dir)?;
        Ok(Self {
            data_dir: data_dir.to_string(),
            catalog: None,
            current_db: None,
        })
    }

    /// Get a reference to the current catalog, or fail if no database is selected.
    ///
    /// # Returns
    ///
    /// * `Result<&BPlusTree>` — the catalog tree.
    fn catalog(&self) -> Result<&BPlusTree> {
        self.catalog
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no database selected"))
    }

    /// Get a mutable reference to the current catalog, or fail if no database is selected.
    ///
    /// # Returns
    ///
    /// * `Result<&mut BPlusTree>` — the catalog tree.
    fn catalog_mut(&mut self) -> Result<&mut BPlusTree> {
        self.catalog
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("no database selected"))
    }

    /// Build a database file path from a name.
    ///
    /// # Arguments
    ///
    /// * `name` - the database name.
    ///
    /// # Returns
    ///
    /// * `String` - the database file path.
    fn db_path(&self, name: &str) -> String {
        format!("{}/{}.db", self.data_dir, name.to_uppercase())
    }

    /// Create a new table with the given name and column definitions.
    ///
    /// ```text
    /// metadata[0..8]  = next_id (u64 LE)
    /// metadata[8..10] = column count (u16 LE)
    /// for each column:
    ///    [2] name_len (u16 LE) [name_len] name [1] type_tag
    ///    if type_tag == 5 (Char): [2] width (u16 LE)
    /// ```
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the table to create.
    /// * `columns` - The column definitions for the table.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn create_table(&mut self, name: &str, columns: &[ColumnDef]) -> Result<()> {
        if self.table_exists(name)? {
            bail!("table {} already exists", name);
        }
        let mut metadata = Vec::new();
        metadata.extend_from_slice(&1u64.to_le_bytes());
        metadata.extend_from_slice(&(columns.len() as u16).to_le_bytes());
        for col in columns {
            let n = col.name.as_bytes();
            metadata.extend_from_slice(&(n.len() as u16).to_le_bytes());
            metadata.extend_from_slice(n);
            metadata.push(type_tag(&col.data_type));
            if let DataType::Char(n) = &col.data_type {
                metadata.extend_from_slice(&(*n as u16).to_le_bytes());
            }
        }
        self.catalog_mut()?
            .create_entry(table_key(name).as_bytes(), &metadata)
    }

    /// Drop a table and its associated data tree.
    ///
    /// # Arguments
    ///
    /// * `name` — the table name to drop.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn drop_table(&mut self, name: &str) -> Result<()> {
        if !self.table_exists(name)? {
            bail!("table {} does not exist", name);
        }
        self.catalog_mut()?.drop_entry(table_key(name).as_bytes())
    }

    /// Check whether a table exists in the catalog.
    ///
    /// # Arguments
    ///
    /// * `name` — the table name.
    ///
    /// # Returns
    ///
    /// * `Result<bool>` — `true` if the table exists.
    fn table_exists(&mut self, name: &str) -> Result<bool> {
        self.catalog()?
            .get_entry(table_key(name).as_bytes())
            .map(|r| r.is_some())
    }

    /// Get a table's B+ tree and column schema from the catalog.
    ///
    /// # Arguments
    ///
    /// * `name` — the table name.
    ///
    /// # Returns
    ///
    /// * `Result<(BPlusTree, Vec<ColumnDef>)>` — the table tree and schema.
    fn get_table(&mut self, name: &str) -> Result<(BPlusTree, Vec<ColumnDef>)> {
        match self.catalog()?.get_entry(table_key(name).as_bytes())? {
            Some((tree, metadata)) => {
                let col_count = u16::from_le_bytes(metadata[8..10].try_into().unwrap()) as usize;
                let mut columns = Vec::with_capacity(col_count);
                let mut off = 10;
                for _ in 0..col_count {
                    let name_len =
                        u16::from_le_bytes(metadata[off..off + 2].try_into().unwrap()) as usize;
                    off += 2;
                    let name = String::from_utf8(metadata[off..off + name_len].to_vec())?;
                    off += name_len;
                    let mut dt = data_type_from_tag(metadata[off])?;
                    off += 1;
                    if matches!(dt, DataType::Char(_)) {
                        let width =
                            u16::from_le_bytes(metadata[off..off + 2].try_into().unwrap()) as usize;
                        off += 2;
                        dt = DataType::Char(width);
                    }
                    columns.push(ColumnDef {
                        name,
                        data_type: dt,
                    });
                }

                Ok((tree, columns))
            }
            None => bail!("table {} does not exist", name),
        }
    }

    /// Read the next auto-increment row ID for a table.
    ///
    /// # Arguments
    ///
    /// * `name` — the table name.
    ///
    /// # Returns
    ///
    /// * `Result<u64>` — the next row ID.
    fn get_next_id(&mut self, name: &str) -> Result<u64> {
        match self.catalog()?.get_entry(table_key(name).as_bytes())? {
            Some((_, metadata)) => Ok(u64::from_le_bytes(metadata[..8].try_into().unwrap())),
            None => bail!("table {} does not exist", name),
        }
    }

    /// Increment the next row ID and sync the root page in the catalog entry.
    ///
    /// # Arguments
    ///
    /// * `name` — the table name.
    /// * `root_page` — the current root page ID of the table tree.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn increment_next_id(&mut self, name: &str, root_page: u32) -> Result<()> {
        let mut value = self
            .catalog()?
            .get(table_key(name).as_bytes())?
            .ok_or_else(|| anyhow::anyhow!("table {} does not exist", name))?;
        let next_id = u64::from_le_bytes(value[4..12].try_into().unwrap());
        value[..4].copy_from_slice(&root_page.to_le_bytes());
        value[4..12].copy_from_slice(&(next_id + 1).to_le_bytes());
        self.catalog_mut()?.put(table_key(name).as_bytes(), &value)
    }

    /// Sync the table root page in the catalog without incrementing the next ID.
    ///
    /// # Arguments
    ///
    /// * `name` — the table name.
    /// * `root_page` — the current root page ID.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn sync_table_root(&mut self, name: &str, root_page: u32) -> Result<()> {
        let mut value = self
            .catalog()?
            .get(table_key(name).as_bytes())?
            .ok_or_else(|| anyhow::anyhow!("table {} does not exist", name))?;
        value[..4].copy_from_slice(&root_page.to_le_bytes());
        self.catalog_mut()?.put(table_key(name).as_bytes(), &value)
    }

    /// Create an index on the specified table and column.
    ///
    /// ```text
    /// metadata[0..4] = table name length (u32)
    /// metadata[4..4+table_len] = table name
    /// metadata[4+table_len..4+table_len+4] = column name length (u32)
    /// metadata[4+table_len+4..] = column name
    /// ```
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the index to create.
    /// * `table` - The name of the table to index.
    /// * `column` - The name of the column to index.
    ///
    /// # Returns
    ///
    /// Whether the index was created successfully
    fn create_index(&mut self, name: &str, table: &str, column: &str) -> Result<()> {
        let mut metadata = Vec::new();
        metadata.extend_from_slice(&(table.len() as u32).to_le_bytes());
        metadata.extend_from_slice(table.as_bytes());
        metadata.extend_from_slice(&(column.len() as u32).to_le_bytes());
        metadata.extend_from_slice(column.as_bytes());
        self.catalog_mut()?
            .create_entry(index_key(name).as_bytes(), &metadata)
    }

    /// Look up an index by name and return its tree plus metadata.
    ///
    /// # Arguments
    ///
    /// * `name` — the index name.
    ///
    /// # Returns
    ///
    /// * `Result<(BPlusTree, (String, String))>` — the index tree and (table, column) pair.
    fn get_index(&mut self, name: &str) -> Result<(BPlusTree, (String, String))> {
        match self.catalog()?.get_entry(index_key(name).as_bytes())? {
            Some((tree, metadata)) => {
                let table_len = u32::from_le_bytes(metadata[..4].try_into().unwrap()) as usize;
                let table = String::from_utf8(metadata[4..4 + table_len].to_vec())?;
                let col_off = 4 + table_len;
                let col_len =
                    u32::from_le_bytes(metadata[col_off..col_off + 4].try_into().unwrap()) as usize;
                let column =
                    String::from_utf8(metadata[col_off + 4..col_off + 4 + col_len].to_vec())?;

                Ok((tree, (table, column)))
            }
            None => bail!("index {} does not exist", name),
        }
    }

    /// Drop an index from the catalog.
    ///
    /// # Arguments
    ///
    /// * `name` — the index name.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — success or error.
    fn drop_index(&mut self, name: &str) -> Result<()> {
        self.catalog_mut()?.drop_entry(index_key(name).as_bytes())
    }

    /// List all indexes on a given table.
    ///
    /// # Arguments
    ///
    /// * `table` — the table name.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<(String, String)>>` — list of (index_name, column_name) pairs.
    fn list_indexes(&mut self, table: &str) -> Result<Vec<(String, String)>> {
        let prefix = b"index:";
        let entries = self.catalog()?.range_scan(prefix, None)?;
        let mut result = Vec::new();
        for (key, value) in entries {
            if !key.starts_with(prefix) {
                continue;
            }

            let table_len = u32::from_le_bytes(value[4..8].try_into().unwrap()) as usize;
            let idx_table = String::from_utf8(value[8..8 + table_len].to_vec())?;
            if idx_table != table {
                continue;
            }
            let col_off = 8 + table_len;
            let col_len =
                u32::from_le_bytes(value[col_off..col_off + 4].try_into().unwrap()) as usize;
            let column = String::from_utf8(value[col_off + 4..col_off + 4 + col_len].to_vec())?;
            let index_name = String::from_utf8(key[prefix.len()..].to_vec())?;
            result.push((index_name, column));
        }

        Ok(result)
    }

    /// Execute a parsed SQL statement and return the result.
    ///
    /// # Arguments
    ///
    /// * `stmt` — the parsed statement to execute.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — the execution result.
    pub(crate) fn execute(&mut self, stmt: Statement) -> Result<QueryResult> {
        match stmt {
            Statement::CreateTable { name, columns } => self.exec_create_table(name, columns),
            Statement::DropTable { name } => self.exec_drop_table(name),
            Statement::CreateIndex {
                name,
                table,
                column,
            } => self.exec_create_index(name, table, column),
            Statement::DropIndex { name } => self.exec_drop_index(name),
            Statement::Insert {
                table,
                columns,
                values,
            } => self.exec_insert(table, columns, values),
            Statement::Select {
                columns,
                tables,
                condition,
            } => self.exec_select(columns, tables, condition),
            Statement::Update {
                table,
                assignments,
                condition,
            } => self.exec_update(table, assignments, condition),
            Statement::Delete { table, condition } => self.exec_delete(table, condition),
            Statement::ShowTables => self.exec_show_tables(),
            Statement::CreateDatabase { name } => self.exec_create_database(name),
            Statement::DropDatabase { name } => self.exec_drop_database(name),
            Statement::ShowDatabases => self.exec_show_databases(),
            Statement::UseDatabase { name } => self.exec_use_database(name),
        }
    }

    // --- DDL ---

    /// Execute a `CREATE TABLE` statement.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the table to create.
    /// * `columns` - The column definitions for the table.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_create_table(&mut self, name: String, columns: Vec<ColumnDef>) -> Result<QueryResult> {
        self.create_table(&name, &columns)?;

        Ok(QueryResult::Success)
    }

    /// Execute a `DROP TABLE` statement.
    ///
    /// # Arguments
    ///
    /// * `name` — the table name to drop.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_drop_table(&mut self, name: String) -> Result<QueryResult> {
        if !self.list_indexes(&name)?.is_empty() {
            bail!("cannot drop table {} because indexes exist on it", name);
        }
        self.drop_table(&name)?;

        Ok(QueryResult::Success)
    }

    /// Execute a `CREATE INDEX` statement, building the index from existing rows.
    ///
    /// # Arguments
    ///
    /// * `index_name` - The name of the index to create.
    /// * `table` - The name of the table to index.
    /// * `column` - The name of the column to index.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_create_index(
        &mut self,
        index_name: String,
        table: String,
        column: String,
    ) -> Result<QueryResult> {
        let (tree, schema) = self.get_table(&table)?;
        let col_idx = schema
            .iter()
            .position(|c| c.name == column)
            .ok_or_else(|| anyhow::anyhow!("column {} not found in table {}", column, table))?;

        match schema[col_idx].data_type {
            DataType::Integer | DataType::Text | DataType::Char(_) => {}
            _ => bail!(
                "cannot create index on column {} of type {:?}",
                column,
                schema[col_idx].data_type
            ),
        }

        self.create_index(&index_name, &table, &column)?;

        let (mut index_tree, _) = self.get_index(&index_name)?;
        let entries = tree.scan()?;
        for (key, value) in &entries {
            let row = deserialize_row(value, schema.len())?;
            let col_value = &row[col_idx];
            let rowid = u64::from_be_bytes(key[..8].try_into().unwrap());
            let mut index_key = encode_value(col_value);
            index_key.extend_from_slice(&rowid.to_le_bytes());
            index_tree.put(&index_key, &[])?;
        }

        Ok(QueryResult::Success)
    }

    /// Execute a `DROP INDEX` statement.
    ///
    /// # Arguments
    ///
    /// * `index_name` - The name of the index to drop.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_drop_index(&mut self, index_name: String) -> Result<QueryResult> {
        self.drop_index(&index_name)?;

        Ok(QueryResult::Success)
    }

    // --- DML ---

    /// Execute an `INSERT` statement.
    ///
    /// # Arguments
    ///
    /// * `table` - The name of the table to insert into.
    /// * `columns` - An optional list of column names corresponding to the provided values (if not provided, values are assumed to be in schema order).
    /// * `values` - The list of values to insert as a new row.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_insert(
        &mut self,
        table: String,
        columns: Option<Vec<String>>,
        values: Vec<Value>,
    ) -> Result<QueryResult> {
        let (mut tree, schema) = self.get_table(&table)?;

        let values = match columns {
            // if columns are specified
            Some(col_names) => {
                if values.len() != col_names.len() {
                    bail!(
                        "column count mismatch: expected {}, got {}",
                        col_names.len(),
                        values.len()
                    );
                }
                let mut full = vec![Value::Null; schema.len()];
                for (col_name, val) in col_names.iter().zip(values.into_iter()) {
                    let idx = schema
                        .iter()
                        .position(|c| c.name == *col_name)
                        .ok_or_else(|| {
                            anyhow::anyhow!("column {} not found in table {}", col_name, table)
                        })?;
                    validate_value(&val, &schema[idx].data_type)?;
                    full[idx] = val;
                }

                full
            }

            // if no columns are specified
            None => {
                if values.len() != schema.len() {
                    bail!(
                        "column count mismatch: expected {}, got {}",
                        schema.len(),
                        values.len()
                    );
                }
                validate_values(&values, &schema)?;

                values
            }
        };

        let mut values = values;
        for (v, col) in values.iter_mut().zip(schema.iter()) {
            if let DataType::Char(_) = col.data_type
                && let Value::Text(s) = v
            {
                *v = Value::Char(std::mem::take(s));
            }
        }

        let next_id = self.get_next_id(&table)?;
        let row_bytes = serialize_row(&values);
        tree.put(&row_key(next_id), &row_bytes)?;
        self.increment_next_id(&table, tree.root_page())?;

        for (idx_name, col_name) in self.list_indexes(&table)? {
            let col_idx = schema.iter().position(|c| c.name == col_name).unwrap();
            let (mut idx_tree, _) = self.get_index(&idx_name)?;
            let mut key = encode_value(&values[col_idx]);
            key.extend_from_slice(&next_id.to_le_bytes());
            idx_tree.put(&key, &[])?;
        }

        Ok(QueryResult::Success)
    }

    /// Execute a `SELECT` statement.
    ///
    /// For single-table queries, uses an index when possible. For multi-table
    /// queries, conditions are pushed down to each table, then a cartesian
    /// product is computed and cross-table conditions applied.
    ///
    /// # Arguments
    ///
    /// * `columns` - The columns to project (or `*` for all).
    /// * `tables` - The table names to query from.
    /// * `condition` - An optional condition to filter rows.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Rows` with projected columns.
    fn exec_select(
        &mut self,
        columns: Columns,
        tables: Vec<String>,
        condition: Option<BoolExpr>,
    ) -> Result<QueryResult> {
        let mut all_schemas: Vec<Vec<ColumnDef>> = Vec::new();
        for table_name in &tables {
            let (_, schema) = self.get_table(table_name)?;
            all_schemas.push(schema);
        }
        let merged_schema = merge_schemas(&all_schemas)?;

        let (local_conds, cross_cond) = split_conditions(&condition, &tables, &all_schemas);

        let mut filtered_per_table: Vec<Vec<Vec<Value>>> = Vec::new();
        for (i, table_name) in tables.iter().enumerate() {
            let rows =
                self.filter_table_rows(table_name, local_conds[i].as_ref(), &all_schemas[i])?;
            filtered_per_table.push(rows);
        }

        let combined = cartesian_product(&filtered_per_table);
        let filtered: Vec<_> = if let Some(ref cc) = cross_cond {
            combined
                .into_iter()
                .filter(|row| evaluate_expr(cc, row, &merged_schema).unwrap_or(false))
                .collect()
        } else {
            combined
        };

        let projected = project_rows(&filtered, &merged_schema, &columns)?;
        Ok(QueryResult::Rows(projected))
    }

    /// Filter a single table's rows, using index when possible.
    ///
    /// # Arguments
    ///
    /// * `table_name` — the table to query.
    /// * `condition` — optional local condition.
    /// * `schema` — the table's schema.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<Vec<Value>>>` — the matching rows.
    fn filter_table_rows(
        &mut self,
        table_name: &str,
        condition: Option<&BoolExpr>,
        schema: &[ColumnDef],
    ) -> Result<Vec<Vec<Value>>> {
        let (tree, _) = self.get_table(table_name)?;
        let single_cond = condition.and_then(as_single_comparison);

        let all_rows = if let Some((col, op, val)) = single_cond {
            let idx_entries = self.list_indexes(table_name)?;
            if can_use_index(col, op, schema, &idx_entries) {
                let idx_name = idx_entries
                    .iter()
                    .find(|(_, c)| *c == col)
                    .map(|(n, _)| n.as_str())
                    .unwrap();
                let (idx_tree, _) = self.get_index(idx_name)?;
                let scanned = index_scan(&tree, &idx_tree, col, op, val, schema)?;
                scanned.into_iter().map(|(_, row)| row).collect()
            } else {
                read_all_rows(&tree, schema)?
            }
        } else {
            read_all_rows(&tree, schema)?
        };

        match condition {
            Some(cond) => Ok(all_rows
                .into_iter()
                .filter(|row| evaluate_expr(cond, row, schema).unwrap_or(false))
                .collect()),
            None => Ok(all_rows),
        }
    }

    /// Execute an `UPDATE` statement.
    ///
    /// Updates matching rows and maintains associated indexes.
    ///
    /// # Arguments
    ///
    /// * `table` - The name of the table to update.
    /// * `assignments` - The list of column assignments to apply to matching rows.
    /// * `condition` - The condition to determine which rows to update.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `RowsAffected(count)`.
    fn exec_update(
        &mut self,
        table: String,
        assignments: Vec<Assignment>,
        condition: BoolExpr,
    ) -> Result<QueryResult> {
        let (mut tree, schema) = self.get_table(&table)?;

        let indexes = self.list_indexes(&table)?;
        let mut affected = 0;

        let single_cond = as_single_comparison(&condition);

        // get rowsid and rows matching the condition, using index if possible
        let rows_and_ids = if let Some((col, op, val)) = single_cond
            && can_use_index(col, op, &schema, &indexes)
        {
            let idx_name = indexes
                .iter()
                .find(|(_, c)| *c == col)
                .map(|(n, _)| n.clone())
                .unwrap();
            let (idx_tree, _) = self.get_index(&idx_name)?;
            index_scan(&tree, &idx_tree, col, op, val, &schema)?
        } else {
            tree.scan()?
                .into_iter()
                .map(|(k, v)| {
                    let rowid = u64::from_be_bytes(k[..8].try_into().unwrap());
                    (rowid, deserialize_row(&v, schema.len()).unwrap())
                })
                .collect()
        };

        for (rowid, mut row) in rows_and_ids {
            if !evaluate_expr(&condition, &row, &schema).unwrap_or(false) {
                continue;
            }

            // update index
            for (idx_name, col_name) in &indexes {
                let col_idx = schema.iter().position(|c| c.name == *col_name).unwrap();
                let old_val = encode_value(&row[col_idx]);

                let changed = assignments.iter().any(|a| a.column == *col_name);
                if changed {
                    let (mut idx_tree, _) = self.get_index(idx_name)?;
                    let new_val = assignments
                        .iter()
                        .find_map(|a| {
                            if a.column == *col_name {
                                Some(&a.value)
                            } else {
                                None
                            }
                        })
                        .unwrap();
                    let mut old_key = old_val.clone();
                    old_key.extend_from_slice(&rowid.to_le_bytes());
                    idx_tree.delete(&old_key)?;

                    let mut new_key = encode_value(new_val);
                    new_key.extend_from_slice(&rowid.to_le_bytes());
                    idx_tree.put(&new_key, &[])?;
                }
            }

            // update row
            for a in &assignments {
                let idx = schema
                    .iter()
                    .position(|c| c.name == a.column)
                    .ok_or_else(|| anyhow::anyhow!("column {} not found", a.column))?;
                validate_value(&a.value, &schema[idx].data_type)?;
                row[idx] = a.value.clone();
            }
            for (v, col) in row.iter_mut().zip(schema.iter()) {
                if let DataType::Char(_) = col.data_type
                    && let Value::Text(s) = v
                {
                    *v = Value::Char(std::mem::take(s));
                }
            }
            tree.put(&row_key(rowid), &serialize_row(&row))?;
            affected += 1;
        }

        if affected > 0 {
            self.sync_table_root(&table, tree.root_page())?;
        }

        Ok(QueryResult::RowsAffected(affected))
    }

    /// Execute a `DELETE` statement.
    ///
    /// Deletes matching rows and maintains associated indexes.
    ///
    /// # Arguments
    ///
    /// * `table` - The name of the table to delete from.
    /// * `condition` - The condition to determine which rows to delete.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `RowsAffected(count)`.
    fn exec_delete(&mut self, table: String, condition: BoolExpr) -> Result<QueryResult> {
        let (mut tree, schema) = self.get_table(&table)?;

        let indexes = self.list_indexes(&table)?;
        let mut affected = 0;

        let single_cond = as_single_comparison(&condition);

        if let Some((col, op, val)) = single_cond
            && can_use_index(col, op, &schema, &indexes)
        {
            let idx_name = indexes
                .iter()
                .find(|(_, c)| *c == col)
                .map(|(n, _)| n.clone())
                .unwrap();
            let (idx_tree, _) = self.get_index(&idx_name)?;
            let scanned = index_scan(&tree, &idx_tree, col, op, val, &schema)?;
            for (rowid, row) in scanned {
                // update indexes
                for (idx_name, col_name) in &indexes {
                    let col_idx = schema.iter().position(|c| c.name == *col_name).unwrap();
                    let (mut idx_tree, _) = self.get_index(idx_name)?;
                    let mut idx_key = encode_value(&row[col_idx]);
                    idx_key.extend_from_slice(&rowid.to_le_bytes());
                    idx_tree.delete(&idx_key)?;
                }
                tree.delete(&row_key(rowid))?;
                affected += 1;
            }
        } else {
            let entries = tree.scan()?;
            for (key, value) in &entries {
                let row = deserialize_row(value, schema.len())?;
                if evaluate_expr(&condition, &row, &schema).unwrap_or(false) {
                    let rowid = u64::from_be_bytes(key[..8].try_into().unwrap());
                    // update indexes
                    for (idx_name, col_name) in &indexes {
                        let col_idx = schema.iter().position(|c| c.name == *col_name).unwrap();
                        let (mut idx_tree, _) = self.get_index(idx_name)?;
                        let mut idx_key = encode_value(&row[col_idx]);
                        idx_key.extend_from_slice(&rowid.to_le_bytes());
                        idx_tree.delete(&idx_key)?;
                    }
                    tree.delete(key)?;
                    affected += 1;
                }
            }
        }

        if affected > 0 {
            self.sync_table_root(&table, tree.root_page())?;
        }

        Ok(QueryResult::RowsAffected(affected))
    }

    /// Execute a `SHOW TABLES` statement.
    ///
    /// Lists all user tables (not indexes) from the catalog.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Rows` with table names.
    fn exec_show_tables(&mut self) -> Result<QueryResult> {
        let entries = self.catalog()?.range_scan(b"table:", None)?;
        let mut names = entries
            .into_iter()
            .filter_map(|(key, _)| {
                if key.starts_with(b"table:") {
                    Some(key[6..].to_vec())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        names.sort();
        let rows: Vec<Vec<Value>> = names
            .into_iter()
            .map(|name| vec![Value::Text(String::from_utf8(name).unwrap())])
            .collect();
        Ok(QueryResult::Rows(rows))
    }

    /// Execute a `CREATE DATABASE` statement.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the database to create.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_create_database(&self, name: String) -> Result<QueryResult> {
        let path = self.db_path(&name);
        if Path::new(&path).exists() {
            bail!("database {} already exists", name);
        }
        BPlusTree::open(&path)?;

        Ok(QueryResult::Success)
    }

    /// Execute a `DROP DATABASE` statement.
    ///
    /// # Arguments
    ///
    /// * `name` - The name of the database to drop.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_drop_database(&mut self, name: String) -> Result<QueryResult> {
        let path = self.db_path(&name);
        if !Path::new(&path).exists() {
            bail!("database {} does not exist", name);
        }
        if self.current_db.as_deref() == Some(&name.to_uppercase()) {
            self.catalog = None;
            self.current_db = None;
        }
        fs::remove_file(&path)?;

        Ok(QueryResult::Success)
    }

    /// Execute a `SHOW DATABASES` statement.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Rows` with database names.
    fn exec_show_databases(&self) -> Result<QueryResult> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            if let Some(stem) = name.strip_suffix(".db") {
                names.push(vec![Value::Text(stem.to_string())]);
            }
        }
        names.sort_by(|a, b| match (&a[0], &b[0]) {
            (Value::Text(a), Value::Text(b)) => a.cmp(b),
            _ => std::cmp::Ordering::Equal,
        });

        Ok(QueryResult::Rows(names))
    }

    /// Execute a `USE name` statement.
    ///
    /// # Returns
    ///
    /// * `Result<QueryResult>` — `Success` on completion.
    fn exec_use_database(&mut self, name: String) -> Result<QueryResult> {
        let path = self.db_path(&name);
        if !Path::new(&path).exists() {
            bail!("database {} does not exist", name);
        }
        self.catalog = Some(BPlusTree::open(&path)?);
        self.current_db = Some(name.to_uppercase());

        Ok(QueryResult::Success)
    }
}

/// Merge column definitions from multiple tables into a single schema.
///
/// # Arguments
///
/// * `schemas` — column definitions from each table.
///
/// # Returns
///
/// * `Result<Vec<ColumnDef>>` — the merged schema, or an error on duplicate column names.
fn merge_schemas(schemas: &[Vec<ColumnDef>]) -> Result<Vec<ColumnDef>> {
    let mut merged = Vec::new();
    for schema in schemas {
        for col in schema {
            if merged.iter().any(|c: &ColumnDef| c.name == col.name) {
                bail!("duplicate column name: {}", col.name);
            }
            merged.push(col.clone());
        }
    }

    Ok(merged)
}

/// Split a boolean expression into per-table local conditions and a cross-table remainder.
///
/// # Arguments
///
/// * `condition` — the full WHERE expression.
/// * `table_names` — the table names in order.
/// * `schemas` — the schemas in the same order as tables.
///
/// # Returns
///
/// * `(Vec<Option<BoolExpr>>, Option<BoolExpr>)` — per-table local conditions and cross-table residual.
fn split_conditions(
    condition: &Option<BoolExpr>,
    table_names: &[String],
    schemas: &[Vec<ColumnDef>],
) -> (Vec<Option<BoolExpr>>, Option<BoolExpr>) {
    let mut locals: Vec<Option<BoolExpr>> = vec![None; table_names.len()];
    let cross = match condition {
        Some(expr) => split_expr(expr, table_names, schemas, &mut locals),
        None => None,
    };

    (locals, cross)
}

/// Recursively split a BoolExpr, collecting local comparisons and building a cross-table residual.
///
/// # Arguments
///
/// * `expr` — the expression to split.
/// * `table_names` — the table names in order.
/// * `schemas` — the schemas in the same order as tables.
/// * `locals` — mutable per-table local conditions being built up.
///
/// # Returns
///
/// * `Option<BoolExpr>` — the cross-table residual expression, or `None` if fully split.
fn split_expr(
    expr: &BoolExpr,
    table_names: &[String],
    schemas: &[Vec<ColumnDef>],
    locals: &mut [Option<BoolExpr>],
) -> Option<BoolExpr> {
    match expr {
        BoolExpr::Comparison { column, op, value } => {
            let owner = find_table(column, table_names, schemas);
            match owner {
                Some(idx) => {
                    let cond = BoolExpr::Comparison {
                        column: column.clone(),
                        op: op.clone(),
                        value: value.clone(),
                    };
                    match locals[idx].take() {
                        Some(existing) => {
                            locals[idx] = Some(BoolExpr::And(Box::new(existing), Box::new(cond)));
                        }
                        None => locals[idx] = Some(cond),
                    }
                    None
                }
                None => Some(expr.clone()),
            }
        }
        BoolExpr::And(left, right) => {
            let l = split_expr(left, table_names, schemas, locals);
            let r = split_expr(right, table_names, schemas, locals);
            match (l, r) {
                (Some(a), Some(b)) => Some(BoolExpr::And(Box::new(a), Box::new(b))),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            }
        }
        BoolExpr::Or(_, _) => {
            // OR cannot be pushed down — pushing each side independently
            // would incorrectly pre-filter rows before the cartesian product.
            // e.g. WHERE s.ssex=0 OR c.cid=1 must evaluate after the join.
            Some(expr.clone())
        }
    }
}

/// Find which table a column belongs to, by name.
///
/// # Arguments
///
/// * `column` — the column name to find.
/// * `table_names` — the table names in order.
/// * `schemas` — the schemas in the same order as tables.
///
/// # Returns
///
/// * `Option<usize>` — the table index, or `None` if the column is not found.
fn find_table(column: &str, _table_names: &[String], schemas: &[Vec<ColumnDef>]) -> Option<usize> {
    schemas
        .iter()
        .position(|s| s.iter().any(|c| c.name == column))
}

/// Compute the cartesian product of multiple row sets.
///
/// # Arguments
///
/// * `rows_sets` — per-table filtered rows.
///
/// # Returns
///
/// * `Vec<Vec<Value>>` — all combinations of rows.
fn cartesian_product(rows_sets: &[Vec<Vec<Value>>]) -> Vec<Vec<Value>> {
    if rows_sets.is_empty() {
        return vec![];
    }
    let mut result: Vec<Vec<Value>> = vec![vec![]];
    for rows in rows_sets {
        let mut next = Vec::new();
        for r in &result {
            for row in rows {
                let mut combined = r.clone();
                combined.extend(row.clone());
                next.push(combined);
            }
        }
        result = next;
    }

    result
}

// --- helpers ---

/// Prefix a table name with `table:` for catalog keys.
///
/// # Arguments
///
/// * `name` - The name of the table.
///
/// # Returns
///
/// A string representing the catalog key for the table.
fn table_key(name: &str) -> String {
    format!("table:{}", name.to_uppercase())
}

/// Prefix an index name with `index:` for catalog keys.
///
/// # Arguments
///
/// * `name` - The name of the index.
///
/// # Returns
///
/// A string representing the catalog key for the index.
fn index_key(name: &str) -> String {
    format!("index:{}", name.to_uppercase())
}

/// Encode a value into bytes for index keys.
///
/// Actually, we only support Integer and Text indexes for now, but this can be extended to other types if needed.
///
/// # Arguments
///
/// * `value` - The value to encode.
///
/// # Returns
///
/// A byte vector representing the encoded value, suitable for index keys.
fn encode_value(value: &Value) -> Vec<u8> {
    match value {
        Value::Integer(v) => {
            let mut bytes = v.to_be_bytes().to_vec();
            bytes[0] ^= 0x80; // flip sign bit for correct ordering
            bytes
        }
        Value::Float(v) => v.to_be_bytes().to_vec(),
        Value::Text(v) => v.as_bytes().to_vec(),
        Value::Char(v) => v.as_bytes().to_vec(),
        Value::Boolean(v) => vec![if *v { 1 } else { 0 }],
        Value::Null => vec![],
    }
}

/// Encode a row ID as an 8-byte big-endian key.
///
/// # Arguments
///
/// * `id` - The row ID to encode.
///
/// # Returns
///
/// A byte vector representing the encoded row ID, suitable for table keys.
fn row_key(id: u64) -> Vec<u8> {
    id.to_be_bytes().to_vec()
}

/// Perform an index scan on the given table and index trees using the specified condition.
///
/// # Arguments
///
/// * `table_tree` - The B+ tree containing the table data.
/// * `index_tree` - The B+ tree containing the index data.
/// * `column` - The column name to filter on.
/// * `op` - The comparison operator.
/// * `value` - The value to compare against.
/// * `schema` - The schema of the table, used to determine column types and positions.
///
/// # Returns
///
/// (rowid, row) pairs matching the condition
fn index_scan(
    table_tree: &BPlusTree,
    index_tree: &BPlusTree,
    column: &str,
    op: &Operator,
    value: &Value,
    schema: &[ColumnDef],
) -> Result<Vec<(u64, Vec<Value>)>> {
    let col_pos = schema.iter().position(|c| c.name == column).unwrap();
    let is_text = matches!(schema[col_pos].data_type, DataType::Text);

    let enc = encode_value(value);

    let (start, end): (Vec<u8>, Option<Vec<u8>>) = match op {
        Operator::Eq => match is_text {
            true => {
                let mut e = enc.clone();
                e.push(0);
                (enc.clone(), Some(e))
            }
            false => {
                let next = match value {
                    Value::Integer(n) => encode_value(&Value::Integer(n.wrapping_add(1))),
                    _ => unreachable!(),
                };
                (enc.clone(), Some(next))
            }
        },
        Operator::Gt => match is_text {
            true => {
                let mut s = enc.clone();
                s.push(0);
                (s, None)
            }
            false => {
                let next = match value {
                    Value::Integer(n) => encode_value(&Value::Integer(n.wrapping_add(1))),
                    _ => unreachable!(),
                };
                (next, None)
            }
        },
        Operator::Lt => (vec![], Some(enc)),
        Operator::Ge => (enc, None),
        Operator::Le => match is_text {
            true => {
                let mut e = enc.clone();
                e.push(0);
                (vec![], Some(e))
            }
            false => {
                let next = match value {
                    Value::Integer(n) => encode_value(&Value::Integer(n.wrapping_add(1))),
                    _ => unreachable!(),
                };
                (vec![], Some(next))
            }
        },
        _ => unreachable!(),
    };

    let idx_rows = index_tree.range_scan(&start, end.as_deref())?;
    let mut result = Vec::with_capacity(idx_rows.len());
    for (k, _) in idx_rows {
        let rowid = u64::from_le_bytes(k[k.len() - 8..].try_into().unwrap());
        let rk = row_key(rowid);
        if let Some(v) = table_tree.get(&rk)? {
            let row = deserialize_row(&v, schema.len())?;
            // ensure the condition is actually satisfied
            if evaluate_condition(column, op, value, &row, schema).unwrap_or(false) {
                result.push((rowid, row));
            }
        }
    }

    Ok(result)
}

/// Determine if we can use an index for the given condition based on the table schema and available indexes.
///
/// The condition includes:
///
/// 1. The operator must be one that can be efficiently supported by an index (e.g., =, >, <, >=, <=).
/// 2. The column referenced in the condition must exist in the table schema.
/// 3. The column's data type must be one that we support indexing on (e.g., Integer, Text).
/// 4. There must be an index defined on that column.
///
/// # Arguments
///
/// * `column` - The column name in the condition.
/// * `op` - The comparison operator.
/// * `schema` - The schema of the table, used to check column existence and data types.
/// * `indexes` - The list of available indexes on the table, used to check for an index on the relevant column.
///
/// # Returns
///
/// `true` if we can use an index for this condition, `false` otherwise.
fn can_use_index(
    column: &str,
    op: &Operator,
    schema: &[ColumnDef],
    indexes: &[(String, String)],
) -> bool {
    op != &Operator::Ne
        && schema.iter().any(|c| c.name == column)
        && matches!(
            schema.iter().find(|c| c.name == column).unwrap().data_type,
            DataType::Integer | DataType::Text
        )
        && indexes.iter().any(|(_, col)| *col == column)
}

/// Read all rows from a table tree into a `Vec<Vec<Value>>`.
///
/// # Arguments
///
/// * `tree` — the table B+ tree.
/// * `schema` — the table schema.
///
/// # Returns
///
/// * `Result<Vec<Vec<Value>>>` — the deserialized rows.
fn read_all_rows(tree: &BPlusTree, schema: &[ColumnDef]) -> Result<Vec<Vec<Value>>> {
    let col_count = schema.len();
    let rows = tree
        .scan()?
        .into_iter()
        .map(|(_, v)| deserialize_row(&v, col_count))
        .collect::<Result<Vec<_>>>()?;

    Ok(rows)
}

/// Project rows to the requested columns.
///
/// # Arguments
///
/// * `rows` — the full rows from the table.
/// * `schema` — the table schema.
/// * `columns` — the column selection (star or list).
///
/// # Returns
///
/// * `Result<Vec<Vec<Value>>>` — the projected rows.
fn project_rows(
    rows: &[Vec<Value>],
    schema: &[ColumnDef],
    columns: &Columns,
) -> Result<Vec<Vec<Value>>> {
    let indices: Vec<usize> = match columns {
        Columns::Star => (0..schema.len()).collect(),
        Columns::List(names) => names
            .iter()
            .map(|n| {
                schema
                    .iter()
                    .position(|c| &c.name == n)
                    .ok_or_else(|| anyhow::anyhow!("column {} not found", n))
            })
            .collect::<Result<_>>()?,
    };

    Ok(rows
        .iter()
        .map(|row| indices.iter().map(|&i| row[i].clone()).collect())
        .collect())
}

/// Evaluate a boolean expression tree against a single row.
///
/// # Arguments
///
/// * `expr` - The boolean expression to evaluate.
/// * `row` - The row of values to evaluate the expression against.
/// * `schema` - The table schema.
///
/// # Returns
///
/// * `Option<bool>` — `Some(true/false)` if evaluation succeeds, `None` on type mismatch.
fn evaluate_expr(expr: &BoolExpr, row: &[Value], schema: &[ColumnDef]) -> Option<bool> {
    match expr {
        BoolExpr::Comparison { column, op, value } => {
            evaluate_condition(column, op, value, row, schema)
        }
        BoolExpr::And(left, right) => match evaluate_expr(left, row, schema) {
            Some(false) => Some(false),
            Some(true) => evaluate_expr(right, row, schema),
            None => None,
        },
        BoolExpr::Or(left, right) => match evaluate_expr(left, row, schema) {
            Some(true) => Some(true),
            Some(false) => evaluate_expr(right, row, schema),
            None => None,
        },
    }
}

/// Evaluate a WHERE condition against a single row.
///
/// # Arguments
///
/// * `column` - The column name.
/// * `op` - The comparison operator.
/// * `cond_val` - The value to compare against.
/// * `row` - The row of values to evaluate the condition against.
/// * `schema` - The table schema, used to find the column index and type for the condition.
///
/// # Returns
///
/// * `Option<bool>` — `Some(true/false)` if evaluation succeeds, `None` on type mismatch.
fn evaluate_condition(
    column: &str,
    op: &Operator,
    cond_val: &Value,
    row: &[Value],
    schema: &[ColumnDef],
) -> Option<bool> {
    let idx = schema.iter().position(|c| c.name == column)?;
    let row_val = &row[idx];

    match op {
        Operator::Eq => Some(values_equal(row_val, cond_val)),
        Operator::Ne => Some(!values_equal(row_val, cond_val)),
        Operator::Gt => compare_values(row_val, cond_val).map(|o| o == Ordering::Greater),
        Operator::Lt => compare_values(row_val, cond_val).map(|o| o == Ordering::Less),
        Operator::Ge => compare_values(row_val, cond_val).map(|o| o != Ordering::Less),
        Operator::Le => compare_values(row_val, cond_val).map(|o| o != Ordering::Greater),
    }
}

/// Extract comparison fields from a BoolExpr, if it is a simple comparison.
///
/// # Arguments
///
/// * `expr` — the boolean expression to inspect.
///
/// # Returns
///
/// * `Option<(&str, &Operator, &Value)>` — the comparison fields, or `None` if the expression
///   is not a simple comparison (e.g., it contains AND/OR).
fn as_single_comparison(expr: &BoolExpr) -> Option<(&str, &Operator, &Value)> {
    match expr {
        BoolExpr::Comparison { column, op, value } => Some((column.as_str(), op, value)),
        _ => None,
    }
}

/// Compare two values for equality (including Null == Null).
///
/// # Arguments
///
/// * `a` - The first value to compare.
/// * `b` - The second value to compare.
///
/// # Returns
///
/// `true` if the values are considered equal, `false` otherwise.
fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Integer(a), Value::Integer(b)) => a == b,
        (Value::Float(a), Value::Float(b)) => a == b,
        (Value::Text(a), Value::Text(b)) => a == b,
        (Value::Char(a), Value::Char(b)) => a == b,
        (Value::Text(a), Value::Char(b)) | (Value::Char(a), Value::Text(b)) => a == b,
        (Value::Boolean(a), Value::Boolean(b)) => a == b,
        _ => false,
    }
}

/// Compare two values for ordering.
///
/// # Arguments
///
/// * `a` - The first value to compare.
/// * `b` - The second value to compare.
///
/// # Returns
///
/// * `Option<Ordering>` — `None` if the types are not comparable.
fn compare_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Integer(a), Value::Integer(b)) => Some(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        (Value::Text(a), Value::Text(b)) => Some(a.cmp(b)),
        (Value::Char(a), Value::Char(b)) => Some(a.cmp(b)),
        (Value::Text(a), Value::Char(b)) | (Value::Char(a), Value::Text(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

/// Validate that each value matches its column's data type.
///
/// # Arguments
///
/// * `values` — the values to validate.
/// * `schema` — the table schema.
///
/// # Returns
///
/// * `Result<()>` — success if all values are valid, error on the first mismatch.
fn validate_values(values: &[Value], schema: &[ColumnDef]) -> Result<()> {
    for (v, col) in values.iter().zip(schema.iter()) {
        validate_value(v, &col.data_type)?;
    }

    Ok(())
}

/// Validate that a single value matches the expected data type.
///
/// # Arguments
///
/// * `value` — the value to validate.
/// * `data_type` — the expected data type.
///
/// # Returns
///
/// * `Result<()>` — success if the value is valid, error on mismatch.
fn validate_value(value: &Value, data_type: &DataType) -> Result<()> {
    match (value, data_type) {
        (Value::Integer(_), DataType::Integer)
        | (Value::Float(_), DataType::Float)
        | (Value::Text(_), DataType::Text)
        | (Value::Char(_), DataType::Char(_))
        | (Value::Boolean(_), DataType::Boolean)
        | (Value::Null, _) => Ok(()),
        (Value::Text(_), DataType::Char(_)) => Ok(()),
        _ => bail!(
            "type mismatch: cannot store {:?} in {:?} column",
            value,
            data_type
        ),
    }
}

/// Map a `DataType` to a single-byte tag for storage.
///
/// # Arguments
///
/// * `dt` — the data type to map.
///
/// # Returns
///
/// * `u8` — the type tag.
fn type_tag(dt: &DataType) -> u8 {
    match dt {
        DataType::Integer => 1,
        DataType::Float => 2,
        DataType::Text => 3,
        DataType::Boolean => 4,
        DataType::Char(_) => 5,
    }
}

/// Map a single-byte tag back to a `DataType`.
///
/// # Arguments
///
/// * `tag` — the type tag byte.
///
/// # Returns
///
/// * `Result<DataType>` — the decoded data type.
fn data_type_from_tag(tag: u8) -> Result<DataType> {
    match tag {
        1 => Ok(DataType::Integer),
        2 => Ok(DataType::Float),
        3 => Ok(DataType::Text),
        4 => Ok(DataType::Boolean),
        5 => Ok(DataType::Char(0)),
        _ => bail!("unknown type tag {}", tag),
    }
}

/// Serialize a row (list of values) into a byte vector for storage.
///
/// # Arguments
///
/// * `values` — the list of values representing the row.
///
/// # Returns
///
/// * `Vec<u8>` — the serialized row bytes.
fn serialize_row(values: &[Value]) -> Vec<u8> {
    let mut buf = Vec::new();
    for v in values {
        serialize_value(v, &mut buf);
    }
    buf
}

/// Serialize a single value, appending to `buf`.
///
/// Format: `[1-byte tag] [data]`
///
/// # Arguments
///
/// * `value` — the value to serialize.
/// * `buf` — the output buffer to append to.
fn serialize_value(value: &Value, buf: &mut Vec<u8>) {
    match value {
        Value::Null => buf.push(0x00),
        Value::Integer(v) => {
            buf.push(0x01);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Float(v) => {
            buf.push(0x02);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        Value::Text(v) => {
            buf.push(0x03);
            let bytes = v.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
        Value::Boolean(v) => {
            buf.push(0x04);
            buf.push(if *v { 1 } else { 0 });
        }
        Value::Char(v) => {
            buf.push(0x05);
            let bytes = v.as_bytes();
            buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            buf.extend_from_slice(bytes);
        }
    }
}

/// Deserialize a byte slice into a row of `col_count` values.
///
/// # Arguments
///
/// * `data` — the serialized row bytes.
/// * `col_count` — the expected number of columns.
///
/// # Returns
///
/// * `Result<Vec<Value>>` — the deserialized row.
fn deserialize_row(mut data: &[u8], col_count: usize) -> Result<Vec<Value>> {
    let mut values = Vec::with_capacity(col_count);
    for _ in 0..col_count {
        let (val, consumed) = deserialize_value(data)?;
        values.push(val);
        data = &data[consumed..];
    }

    Ok(values)
}

/// Deserialize a single value from the start of a byte slice.
///
/// # Arguments
///
/// * `data` — the serialized bytes, starting at the value.
///
/// # Returns
///
/// * `Result<(Value, usize)>` — the deserialized value and the number of bytes consumed.
fn deserialize_value(data: &[u8]) -> Result<(Value, usize)> {
    if data.is_empty() {
        bail!("unexpected end of data while deserializing value");
    }

    match data[0] {
        0x00 => Ok((Value::Null, 1)),
        0x01 => {
            ensure_len(data, 9)?;
            let v = i64::from_le_bytes(data[1..9].try_into().unwrap());
            Ok((Value::Integer(v), 9))
        }
        0x02 => {
            ensure_len(data, 9)?;
            let v = f64::from_le_bytes(data[1..9].try_into().unwrap());
            Ok((Value::Float(v), 9))
        }
        0x03 => {
            ensure_len(data, 5)?;
            let len = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
            ensure_len(data, 5 + len)?;
            let s = String::from_utf8(data[5..5 + len].to_vec())?;
            Ok((Value::Text(s), 5 + len))
        }
        0x04 => {
            ensure_len(data, 2)?;
            let v = data[1] != 0;
            Ok((Value::Boolean(v), 2))
        }
        0x05 => {
            ensure_len(data, 5)?;
            let len = u32::from_le_bytes(data[1..5].try_into().unwrap()) as usize;
            ensure_len(data, 5 + len)?;
            let s = String::from_utf8(data[5..5 + len].to_vec())?;
            Ok((Value::Char(s), 5 + len))
        }
        tag => bail!("unknown value tag {}", tag),
    }
}

/// Check that `data` has at least `needed` bytes, returning an error otherwise.
///
/// # Arguments
///
/// * `data` - The byte slice to check.
/// * `needed` - The minimum number of bytes required.
///
/// # Returns
///
/// * `Result<()>` - `Ok(())` if the length is sufficient, or an error if it's too short.
fn ensure_len(data: &[u8], needed: usize) -> Result<()> {
    if data.len() < needed {
        bail!(
            "unexpected end of data: need {} bytes, have {}",
            needed,
            data.len()
        );
    }

    Ok(())
}

// --- tests ---

#[cfg(test)]
mod tests {
    use super::*;

    fn cleanup(path: &str) {
        let _ = std::fs::remove_file(path);
    }

    fn engine_path(name: &str) -> String {
        format!("data/test_engine_{}.db", name)
    }

    fn open_engine(name: &str) -> Engine {
        let path = engine_path(name);
        Engine {
            data_dir: "data".to_string(),
            catalog: Some(BPlusTree::open(&path).unwrap()),
            current_db: Some(name.to_string()),
        }
    }

    #[test]
    fn test_create_and_drop_table() {
        let mut engine = open_engine("create_drop");
        exec_create(
            &mut engine,
            "t1",
            &[("id", DataType::Integer), ("name", DataType::Text)],
        );
        assert!(engine.table_exists("t1").unwrap());
        engine
            .execute(Statement::DropTable { name: "t1".into() })
            .unwrap();
        assert!(!engine.table_exists("t1").unwrap());
        cleanup(&engine_path("create_drop"));
    }

    #[test]
    fn test_index_select() {
        let raw_name = "index_select";
        let mut engine = open_engine(raw_name);
        let name = "t";
        exec_create(
            &mut engine,
            name,
            &[("id", DataType::Integer), ("name", DataType::Text)],
        );

        for i in 0..100000 {
            engine
                .execute(Statement::Insert {
                    table: name.into(),
                    columns: None,
                    values: vec![Value::Integer(i), Value::Text(format!("u{}", i))],
                })
                .unwrap();
        }

        engine
            .execute(Statement::CreateIndex {
                name: "idx_name".into(),
                table: name.into(),
                column: "name".into(),
            })
            .unwrap();
        drop(engine);

        let search_val = Value::Text("u1500".into());

        let mut engine = open_engine(raw_name);
        let t0 = std::time::Instant::now();
        for _ in 0..50 {
            engine
                .execute(Statement::Select {
                    columns: Columns::Star,
                    tables: vec![name.into()],
                    condition: Some(BoolExpr::Comparison {
                        column: "name".into(),
                        op: Operator::Eq,
                        value: search_val.clone(),
                    }),
                })
                .unwrap();
        }
        let with_index_us = t0.elapsed().as_micros() / 50;

        engine
            .execute(Statement::DropIndex {
                name: "idx_name".into(),
            })
            .unwrap();
        drop(engine);

        let mut engine = open_engine(raw_name);
        let t1 = std::time::Instant::now();
        for _ in 0..50 {
            engine
                .execute(Statement::Select {
                    columns: Columns::Star,
                    tables: vec![name.into()],
                    condition: Some(BoolExpr::Comparison {
                        column: "name".into(),
                        op: Operator::Eq,
                        value: search_val.clone(),
                    }),
                })
                .unwrap();
        }
        let without_index_us = t1.elapsed().as_micros() / 50;

        eprintln!("index:   {} μs/query", with_index_us);
        eprintln!("no idx:  {} μs/query", without_index_us);

        cleanup(&engine_path(raw_name));
    }

    #[test]
    fn test_select_with_where() {
        let mut engine = open_engine("select_where");
        let name = "test_sw";
        exec_create(
            &mut engine,
            name,
            &[("id", DataType::Integer), ("age", DataType::Integer)],
        );

        engine
            .execute(Statement::Insert {
                table: name.into(),
                columns: None,
                values: vec![Value::Integer(1), Value::Integer(20)],
            })
            .unwrap();
        engine
            .execute(Statement::Insert {
                table: name.into(),
                columns: None,
                values: vec![Value::Integer(2), Value::Integer(30)],
            })
            .unwrap();
        engine
            .execute(Statement::Insert {
                table: name.into(),
                columns: None,
                values: vec![Value::Integer(3), Value::Integer(25)],
            })
            .unwrap();

        let result = engine
            .execute(Statement::Select {
                columns: Columns::Star,
                tables: vec![name.into()],
                condition: Some(BoolExpr::Comparison {
                    column: "age".into(),
                    op: Operator::Gt,
                    value: Value::Integer(20),
                }),
            })
            .unwrap();

        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
                assert!(
                    rows[0][0] == Value::Integer(2) && rows[1][0] == Value::Integer(3)
                        || rows[0][0] == Value::Integer(3) && rows[1][0] == Value::Integer(2)
                );
            }
            _ => panic!("expected Rows"),
        }
        cleanup(&engine_path("select_where"));
    }

    #[test]
    fn test_select_column_projection() {
        let mut engine = open_engine("select_proj");
        let name = "test_proj";
        exec_create(
            &mut engine,
            name,
            &[
                ("id", DataType::Integer),
                ("name", DataType::Text),
                ("age", DataType::Integer),
            ],
        );

        engine
            .execute(Statement::Insert {
                table: name.into(),
                columns: None,
                values: vec![
                    Value::Integer(1),
                    Value::Text("alice".into()),
                    Value::Integer(18),
                ],
            })
            .unwrap();

        let result = engine
            .execute(Statement::Select {
                columns: Columns::List(vec!["name".into(), "age".into()]),
                tables: vec![name.into()],
                condition: None,
            })
            .unwrap();

        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 2);
                assert_eq!(rows[0][0], Value::Text("alice".into()));
                assert_eq!(rows[0][1], Value::Integer(18));
            }
            _ => panic!("expected Rows"),
        }
        cleanup(&engine_path("select_proj"));
    }

    #[test]
    fn test_update() {
        let mut engine = open_engine("update");
        let name = "test_upd";
        exec_create(
            &mut engine,
            name,
            &[("id", DataType::Integer), ("name", DataType::Text)],
        );

        engine
            .execute(Statement::Insert {
                table: name.into(),
                columns: None,
                values: vec![Value::Integer(1), Value::Text("alice".into())],
            })
            .unwrap();
        engine
            .execute(Statement::Insert {
                table: name.into(),
                columns: None,
                values: vec![Value::Integer(2), Value::Text("bob".into())],
            })
            .unwrap();

        let result = engine
            .execute(Statement::Update {
                table: name.into(),
                assignments: vec![Assignment {
                    column: "name".into(),
                    value: Value::Text("charlie".into()),
                }],
                condition: BoolExpr::Comparison {
                    column: "id".into(),
                    op: Operator::Eq,
                    value: Value::Integer(1),
                },
            })
            .unwrap();

        assert!(matches!(result, QueryResult::RowsAffected(1)));

        let result = engine
            .execute(Statement::Select {
                columns: Columns::Star,
                tables: vec![name.into()],
                condition: None,
            })
            .unwrap();

        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
                assert!(
                    rows[0][1] == Value::Text("charlie".into())
                        && rows[1][1] == Value::Text("bob".into())
                        || rows[0][1] == Value::Text("bob".into())
                            && rows[1][1] == Value::Text("charlie".into())
                );
            }
            _ => panic!("expected Rows"),
        }
        cleanup(&engine_path("update"));
    }

    #[test]
    fn test_delete() {
        let mut engine = open_engine("engine_delete");
        let name = "test_del";
        exec_create(
            &mut engine,
            name,
            &[("id", DataType::Integer), ("val", DataType::Integer)],
        );

        for i in 1..=3 {
            engine
                .execute(Statement::Insert {
                    table: name.into(),
                    columns: None,
                    values: vec![Value::Integer(i), Value::Integer(i * 10)],
                })
                .unwrap();
        }

        let result = engine
            .execute(Statement::Delete {
                table: name.into(),
                condition: BoolExpr::Comparison {
                    column: "id".into(),
                    op: Operator::Gt,
                    value: Value::Integer(1),
                },
            })
            .unwrap();

        assert!(matches!(result, QueryResult::RowsAffected(2)));

        let result = engine
            .execute(Statement::Select {
                columns: Columns::Star,
                tables: vec![name.into()],
                condition: None,
            })
            .unwrap();

        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0][0], Value::Integer(1));
            }
            _ => panic!("expected Rows"),
        }
        cleanup(&engine_path("engine_delete"));
    }

    #[test]
    fn test_create_and_drop_index() {
        let mut engine = open_engine("index");
        let name = "test_idx";
        exec_create(
            &mut engine,
            name,
            &[("id", DataType::Integer), ("name", DataType::Text)],
        );

        let result = engine.execute(Statement::CreateIndex {
            name: "idx_name".into(),
            table: name.into(),
            column: "name".into(),
        });
        assert!(result.is_ok());

        let result = engine.execute(Statement::DropIndex {
            name: "idx_name".into(),
        });
        assert!(result.is_ok());

        cleanup(&engine_path("index"));
    }

    #[test]
    fn test_duplicate_table_error() {
        let mut engine = open_engine("dup_table");
        let name = "test_dup";
        exec_create(&mut engine, name, &[("id", DataType::Integer)]);

        let result = engine.execute(Statement::CreateTable {
            name: name.into(),
            columns: vec![col("x", DataType::Integer)],
        });
        assert!(result.is_err());

        cleanup(&engine_path("dup_table"));
    }

    #[test]
    fn test_column_count_mismatch() {
        let mut engine = open_engine("col_mismatch");
        let name = "test_ccm";
        exec_create(&mut engine, name, &[("id", DataType::Integer)]);

        let result = engine.execute(Statement::Insert {
            table: name.into(),
            columns: None,
            values: vec![Value::Integer(1), Value::Text("extra".into())],
        });
        assert!(result.is_err());

        cleanup(&engine_path("col_mismatch"));
    }

    #[test]
    fn test_type_mismatch() {
        let mut engine = open_engine("type_mismatch");
        let name = "test_tm";
        exec_create(&mut engine, name, &[("id", DataType::Integer)]);

        let result = engine.execute(Statement::Insert {
            table: name.into(),
            columns: None,
            values: vec![Value::Text("not int".into())],
        });
        assert!(result.is_err());

        cleanup(&engine_path("type_mismatch"));
    }

    #[test]
    fn test_show_tables() {
        let mut engine = open_engine("show_tables");
        exec_create(&mut engine, "t1", &[("id", DataType::Integer)]);
        exec_create(&mut engine, "t2", &[("name", DataType::Text)]);

        let result = engine.execute(Statement::ShowTables).unwrap();
        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 2);
                let mut names: Vec<_> = rows
                    .iter()
                    .map(|r| match &r[0] {
                        Value::Text(s) => s.clone(),
                        _ => panic!("expected Text"),
                    })
                    .collect();
                names.sort();
                assert_eq!(names, vec!["T1".to_string(), "T2".to_string()]);
            }
            _ => panic!("expected Rows"),
        }
        cleanup(&engine_path("show_tables"));
    }

    #[test]
    fn test_select_multi_table() {
        let mut engine = open_engine("select_multi");
        exec_create(
            &mut engine,
            "s",
            &[("sname", DataType::Text), ("ssex", DataType::Integer)],
        );
        exec_create(
            &mut engine,
            "c",
            &[("cid", DataType::Integer), ("cname", DataType::Text)],
        );

        engine
            .execute(Statement::Insert {
                table: "s".into(),
                columns: None,
                values: vec![Value::Text("a".into()), Value::Integer(1)],
            })
            .unwrap();
        engine
            .execute(Statement::Insert {
                table: "c".into(),
                columns: None,
                values: vec![Value::Integer(1), Value::Text("db".into())],
            })
            .unwrap();

        let result = engine
            .execute(Statement::Select {
                columns: Columns::Star,
                tables: vec!["s".into(), "c".into()],
                condition: None,
            })
            .unwrap();

        match result {
            QueryResult::Rows(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 4);
                assert_eq!(rows[0][0], Value::Text("a".into()));
                assert_eq!(rows[0][1], Value::Integer(1));
                assert_eq!(rows[0][2], Value::Integer(1));
                assert_eq!(rows[0][3], Value::Text("db".into()));
            }
            _ => panic!("expected Rows"),
        }
        cleanup(&engine_path("select_multi"));
    }

    // --- test helpers ---

    fn col(name: &str, dt: DataType) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            data_type: dt,
        }
    }

    fn exec_create(engine: &mut Engine, name: &str, cols: &[(&str, DataType)]) {
        let columns: Vec<ColumnDef> = cols.iter().map(|(n, dt)| col(n, dt.clone())).collect();
        engine
            .execute(Statement::CreateTable {
                name: name.into(),
                columns,
            })
            .unwrap();
    }
}
