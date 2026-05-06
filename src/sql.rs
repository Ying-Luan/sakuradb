//! A simple SQL parser for a subset of SQL statements.
//!
//! It accepts SQL strings and produces an abstract syntax tree (AST).

mod ast;
mod lexer;
mod parser;

pub(crate) use ast::{
    Assignment, BoolExpr, ColumnDef, Columns, DataType, Operator, Statement, Value,
};

use anyhow::Result;

use lexer::Lexer;
use parser::Parser;

/// Parse a SQL string into a `Statement`.
///
/// # Arguments
///
/// * `input` - the SQL string to parse.
///
/// # Returns
///
/// * `Result<Statement>` — the parsed statement on success.
pub(crate) fn parser(input: &str) -> Result<Statement> {
    let mut lexer = Lexer::new(input);
    let tokens = lexer.tokenize()?;

    let mut parser = Parser::new(tokens);
    parser.parse()
}
