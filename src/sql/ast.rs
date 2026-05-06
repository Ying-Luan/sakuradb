//! Abstract syntax tree (AST) definitions for SQL statements.

/// SQL data types.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DataType {
    /// Signed 64-bit integer.
    Integer,
    /// 64-bit floating point.
    Float,
    /// Variable-length string.
    Text,
    /// True or false.
    Boolean,
    /// Fixed-length character string with maximum width.
    Char(usize),
}

/// A literal value in a SQL expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    /// Signed 64-bit integer literal.
    Integer(i64),
    /// 64-bit floating point literal.
    Float(f64),
    /// String literal.
    Text(String),
    /// Boolean literal.
    Boolean(bool),
    /// NULL literal.
    Null,
    /// Fixed-length character string literal.
    Char(String),
}

/// Comparison operators used in WHERE clause conditions.
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Operator {
    /// =
    Eq,
    /// !=
    Ne,
    /// >
    Gt,
    /// <
    Lt,
    /// >=
    Ge,
    /// <=
    Le,
}

/// A boolean expression tree for WHERE clauses.
///
/// Supports comparison expressions, logical AND/OR combinations, and parenthesized nesting.
#[derive(Clone)]
pub(crate) enum BoolExpr {
    /// A comparison expression, e.g., `age > 18`.
    Comparison {
        /// Column name to compare.
        column: String,
        /// Comparison operator.
        op: Operator,
        /// Value to compare against.
        value: Value,
    },
    /// Logical AND of two boolean expressions.
    And(Box<BoolExpr>, Box<BoolExpr>),
    /// Logical OR of two boolean expressions.
    Or(Box<BoolExpr>, Box<BoolExpr>),
}

/// Column definition in a CREATE TABLE statement.
#[derive(Clone)]
pub(crate) struct ColumnDef {
    /// Column name.
    pub(crate) name: String,
    /// Column data type.
    pub(crate) data_type: DataType,
}

/// Represents the columns to select in a SELECT statement.
#[derive(Debug, PartialEq)]
pub(crate) enum Columns {
    /// Select all columns (`*`).
    Star,
    /// Select specific columns by name.
    List(Vec<String>),
}

/// Represents an assignment in an UPDATE statement, e.g., `column = value`.
pub(crate) struct Assignment {
    /// Column name to update.
    pub(crate) column: String,
    /// New value to assign.
    pub(crate) value: Value,
}

/// Top-level SQL statement.
pub(crate) enum Statement {
    /// `CREATE TABLE name (col type, ...)`
    CreateTable {
        /// Table name.
        name: String,
        /// Column definitions.
        columns: Vec<ColumnDef>,
    },
    /// `DROP TABLE name`
    DropTable {
        /// Table name.
        name: String,
    },
    /// `CREATE INDEX name ON table (col)`
    CreateIndex {
        /// Index name.
        name: String,
        /// Target table.
        table: String,
        /// Target column.
        column: String,
    },
    /// `DROP INDEX name`
    DropIndex {
        /// Index name.
        name: String,
    },
    /// `INSERT INTO table [(col, ...)] VALUES (val, ...)`
    Insert {
        /// Target table.
        table: String,
        /// Optional column name list. `None` means all columns in schema order.
        columns: Option<Vec<String>>,
        /// Values in column order (if `columns` is `None`) or matching the column list.
        values: Vec<Value>,
    },
    /// `SELECT cols FROM t1 [, t2, ...] [WHERE condition]`
    Select {
        /// Columns to project.
        columns: Columns,
        /// Source tables (single table or comma-separated).
        tables: Vec<String>,
        /// Optional WHERE clause.
        condition: Option<BoolExpr>,
    },
    /// `UPDATE table SET col = val, ... WHERE condition`
    Update {
        /// Target table.
        table: String,
        /// Column assignments.
        assignments: Vec<Assignment>,
        /// WHERE clause.
        condition: BoolExpr,
    },
    /// `DELETE FROM table WHERE condition`
    Delete {
        /// Target table.
        table: String,
        /// WHERE clause.
        condition: BoolExpr,
    },
    /// `SHOW TABLES`
    ShowTables,
    /// `CREATE DATABASE name`
    CreateDatabase {
        /// Database name.
        name: String,
    },
    /// `DROP DATABASE name`
    DropDatabase {
        /// Database name.
        name: String,
    },
    /// `SHOW DATABASES`
    ShowDatabases,
    /// `USE name`
    UseDatabase {
        /// Database name.
        name: String,
    },
}
