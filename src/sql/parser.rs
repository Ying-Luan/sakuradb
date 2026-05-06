//! A recursive descent parser.
//!
//! The parser supports DDL and DML statements: `CREATE TABLE` / `CREATE INDEX` / `CREATE DATABASE`,
//! `DROP TABLE` / `DROP INDEX` / `DROP DATABASE`, `INSERT`, `SELECT`, `UPDATE`, `DELETE`,
//! `SHOW TABLES` / `SHOW DATABASES`, and `USE`.
//!
//! WHERE conditions support composite boolean expressions with `AND`, `OR`, and parenthesized sub-expressions,
//! in addition to simple comparison operators (`=`, `!=`, `<`, `>`, `<=`, `>=`).

use anyhow::{Result, bail};

use crate::sql::{
    ast::{Assignment, BoolExpr, ColumnDef, Columns, DataType, Operator, Statement, Value},
    lexer::Token,
};

/// A recursive descent parser for a simple SQL-like language, supporting the following statements:
///
/// ```text
///    parse()
///      ├─ CREATE ─┬─ TABLE    → parse_create_table()
///      │          ├─ INDEX    → parse_create_index()
///      │          └─ DATABASE → parse_create_database()
///      ├─ DROP   ─┬─ TABLE    → parse_drop_table()
///      │          ├─ INDEX    → parse_drop_index()
///      │          └─ DATABASE → parse_drop_database()
///      ├─ INSERT → parse_insert()
///      ├─ SELECT → parse_select()
///      ├─ UPDATE → parse_update()
///      ├─ DELETE → parse_delete()
///      ├─ SHOW   → parse_show()
///      └─ USE    → parse_use()
///
///    parse_create_table()
///      → Identifier → LParen → parse_column_defs() → RParen → Semicolon
///    parse_column_defs()
///      → Identifier DataType {, Identifier DataType}
///
///    parse_create_index()
///      → Identifier → On → Identifier → LParen → Identifier → RParen → Semicolon
///
///    parse_create_database()
///      → Identifier → Semicolon
///
///    parse_drop_table()
///      → Identifier → Semicolon
///
///    parse_drop_index()
///      → Identifier → Semicolon
///
///    parse_drop_database()
///      → Identifier → Semicolon
///
///    parse_insert()
///      → Into → Identifier → [(col, ...)] → Values → LParen → parse_value_list() → RParen → Semicolon
///    parse_value_list()
///      → parse_value() {, parse_value()}
///    parse_value()
///      → IntLiteral / FloatLiteral / StringLiteral / True / False / Null
///
///    parse_select()
///      → parse_columns() → From → Identifier {, Identifier} → [Where → parse_condition()] → Semicolon
///    parse_columns()
///      → Star | Identifier {, Identifier}
///
///    parse_update()
///      → Identifier → Set → parse_assignments() → Where → parse_condition() → Semicolon
///    parse_assignments()
///      → Identifier Eq parse_value() {, Identifier Eq parse_value()}
///
///    parse_delete()
///      → From → Identifier → Where → parse_condition() → Semicolon
///
///    parse_show()
///      → Tables → Semicolon
///      → Databases → Semicolon
///
///    parse_use()
///      → Identifier → Semicolon
///
///    parse_condition()
///      → parse_or_expr()
///    parse_or_expr()
///      → parse_and_expr() {Or parse_and_expr()}
///    parse_and_expr()
///      → parse_atom() {And parse_atom()}
///    parse_atom()
///      → LParen parse_or_expr() RParen | parse_comparison()
///    parse_comparison()
///      → Identifier Operator parse_value()
/// ```
///
pub(crate) struct Parser {
    /// Token stream from the lexer.
    tokens: Vec<Token>,
    /// Current position in the token stream.
    pos: usize,
}

impl Parser {
    /// Create a new parser from a token stream.
    ///
    /// # Arguments
    ///
    /// * `tokens` — the list of tokens to parse.
    ///
    /// # Returns
    ///
    /// * `Parser` — the initialized parser.
    pub(crate) fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    /// Parse the token stream into a `Statement`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — the parsed statement on success.
    pub(crate) fn parse(&mut self) -> Result<Statement> {
        match self.peek() {
            Some(Token::Create) => self.parse_create(),
            Some(Token::Drop) => self.parse_drop(),
            Some(Token::Insert) => self.parse_insert(),
            Some(Token::Select) => self.parse_select(),
            Some(Token::Update) => self.parse_update(),
            Some(Token::Delete) => self.parse_delete(),
            Some(Token::Show) => self.parse_show(),
            Some(Token::Use) => self.parse_use(),
            Some(tok) => bail!("unexpected token {:?} at position {}", tok, self.pos),
            None => bail!("empty input"),
        }
    }

    // --- CREATE ---

    /// Parse a `CREATE TABLE`, `CREATE INDEX`, or `CREATE DATABASE` statement.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `CreateTable`, `CreateIndex`, or `CreateDatabase`.
    fn parse_create(&mut self) -> Result<Statement> {
        self.expect(Token::Create)?;
        match self.peek() {
            Some(Token::Table) => self.parse_create_table(),
            Some(Token::Index) => self.parse_create_index(),
            Some(Token::Database) => self.parse_create_database(),
            Some(tok) => {
                bail!(
                    "expected TABLE, INDEX, or DATABASE, found {:?} at {}",
                    tok,
                    self.pos
                )
            }
            None => bail!(
                "expected TABLE, INDEX, or DATABASE, found EOF at {}",
                self.pos
            ),
        }
    }

    /// Parse `TABLE name (col type, ...)`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `CreateTable` with name and column definitions.
    fn parse_create_table(&mut self) -> Result<Statement> {
        self.expect(Token::Table)?;
        let name = self.consume_identifier("table name")?;
        self.expect(Token::LParen)?;
        let columns = self.parse_column_defs()?;
        self.expect(Token::RParen)?;
        self.expect(Token::Semicolon)?;
        Ok(Statement::CreateTable { name, columns })
    }

    /// Parse a comma-separated list of `name type` pairs.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<ColumnDef>>` — the column definitions.
    fn parse_column_defs(&mut self) -> Result<Vec<ColumnDef>> {
        let mut columns = Vec::new();
        loop {
            let name = self.consume_identifier("column name")?;
            let data_type = self.parse_data_type()?;
            columns.push(ColumnDef { name, data_type });
            if self.peek() != Some(&Token::Comma) {
                break;
            }
            self.advance(); // consume comma
        }
        Ok(columns)
    }

    /// Parse a single data type keyword.
    ///
    /// # Returns
    ///
    /// * `Result<DataType>` — the matched data type.
    fn parse_data_type(&mut self) -> Result<DataType> {
        let tok = self.advance();
        match tok {
            Token::Integer => Ok(DataType::Integer),
            Token::Float => Ok(DataType::Float),
            Token::Text => Ok(DataType::Text),
            Token::Boolean => Ok(DataType::Boolean),
            Token::Char => {
                self.expect(Token::LParen)?;
                let n = match self.advance() {
                    Token::IntLiteral(n) => n as usize,
                    tok => bail!(
                        "expected integer for CHAR width, found {:?} at {}",
                        tok,
                        self.pos - 1
                    ),
                };
                self.expect(Token::RParen)?;
                Ok(DataType::Char(n))
            }
            _ => bail!("expected a data type, found {:?} at {}", tok, self.pos - 1),
        }
    }

    /// Parse `INDEX name ON table (col)`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `CreateIndex`.
    fn parse_create_index(&mut self) -> Result<Statement> {
        self.expect(Token::Index)?;
        let name = self.consume_identifier("index name")?;
        self.expect(Token::On)?;
        let table = self.consume_identifier("table name")?;
        self.expect(Token::LParen)?;
        let column = self.consume_identifier("column name")?;
        self.expect(Token::RParen)?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::CreateIndex {
            name,
            table,
            column,
        })
    }

    /// Parse `DATABASE name`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `CreateDatabase`.
    fn parse_create_database(&mut self) -> Result<Statement> {
        self.expect(Token::Database)?;
        let name = self.consume_identifier("database name")?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::CreateDatabase { name })
    }

    // --- DROP ---

    /// Parse a `DROP TABLE`, `DROP INDEX`, or `DROP DATABASE` statement.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `DropTable`, `DropIndex`, or `DropDatabase`.
    fn parse_drop(&mut self) -> Result<Statement> {
        self.expect(Token::Drop)?;
        match self.peek() {
            Some(Token::Table) => self.parse_drop_table(),
            Some(Token::Index) => self.parse_drop_index(),
            Some(Token::Database) => self.parse_drop_database(),
            Some(tok) => {
                bail!(
                    "expected TABLE, INDEX, or DATABASE, found {:?} at {}",
                    tok,
                    self.pos
                )
            }
            None => bail!(
                "expected TABLE, INDEX, or DATABASE, found EOF at {}",
                self.pos
            ),
        }
    }

    /// Parse `TABLE name`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `DropTable`.
    fn parse_drop_table(&mut self) -> Result<Statement> {
        self.expect(Token::Table)?;
        let name = self.consume_identifier("table name")?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::DropTable { name })
    }

    /// Parse `INDEX name`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `DropIndex`.
    fn parse_drop_index(&mut self) -> Result<Statement> {
        self.expect(Token::Index)?;
        let name = self.consume_identifier("index name")?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::DropIndex { name })
    }

    /// Parse `DATABASE name`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `DropDatabase`.
    fn parse_drop_database(&mut self) -> Result<Statement> {
        self.expect(Token::Database)?;
        let name = self.consume_identifier("database name")?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::DropDatabase { name })
    }

    // --- INSERT ---

    /// Parse `INTO table [(col, ...)] VALUES (val, ...)`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `Insert`.
    fn parse_insert(&mut self) -> Result<Statement> {
        self.expect(Token::Insert)?;
        self.expect(Token::Into)?;
        let table = self.consume_identifier("table name")?;

        let columns = if self.peek() == Some(&Token::LParen) {
            let cols = self.parse_column_list()?;
            Some(cols)
        } else {
            None
        };

        self.expect(Token::Values)?;
        self.expect(Token::LParen)?;
        let values = self.parse_value_list()?;
        self.expect(Token::RParen)?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::Insert {
            table,
            columns,
            values,
        })
    }

    /// Parse a comma-separated list of column names inside parentheses.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<String>>` — the column names.
    fn parse_column_list(&mut self) -> Result<Vec<String>> {
        self.expect(Token::LParen)?;
        let mut cols = Vec::new();
        loop {
            cols.push(self.consume_identifier("column name")?);
            if self.peek() != Some(&Token::Comma) {
                break;
            }
            self.advance();
        }
        self.expect(Token::RParen)?;

        Ok(cols)
    }

    /// Parse a comma-separated list of values.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<Value>>` — the value list.
    fn parse_value_list(&mut self) -> Result<Vec<Value>> {
        let mut values = Vec::new();
        loop {
            values.push(self.parse_value()?);
            if self.peek() != Some(&Token::Comma) {
                break;
            }
            self.advance();
        }

        Ok(values)
    }

    /// Parse a single literal value.
    ///
    /// # Returns
    ///
    /// * `Result<Value>` — the parsed value.
    fn parse_value(&mut self) -> Result<Value> {
        let tok = self.advance();
        match tok {
            Token::IntLiteral(n) => Ok(Value::Integer(n)),
            Token::FloatLiteral(f) => Ok(Value::Float(f)),
            Token::StringLiteral(s) => Ok(Value::Text(s)),
            Token::True => Ok(Value::Boolean(true)),
            Token::False => Ok(Value::Boolean(false)),
            Token::Null => Ok(Value::Null),
            _ => bail!("expected a value, found {:?} at {}", tok, self.pos - 1),
        }
    }

    // --- SELECT ---

    /// Parse `cols FROM table [, table ...] [WHERE condition]`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `Select`.
    fn parse_select(&mut self) -> Result<Statement> {
        self.expect(Token::Select)?;
        let columns = self.parse_columns()?;
        self.expect(Token::From)?;
        let mut tables = vec![self.consume_identifier("table name")?];
        while self.peek() == Some(&Token::Comma) {
            self.advance();
            tables.push(self.consume_identifier("table name")?);
        }
        let condition = if self.peek() == Some(&Token::Where) {
            self.advance();
            Some(self.parse_condition()?)
        } else {
            None
        };
        self.expect(Token::Semicolon)?;

        Ok(Statement::Select {
            columns,
            tables,
            condition,
        })
    }

    /// Parse `*` or a comma-separated column list.
    ///
    /// # Returns
    ///
    /// * `Result<Columns>` — `Star` or `List(...)`.
    fn parse_columns(&mut self) -> Result<Columns> {
        if self.peek() == Some(&Token::Star) {
            self.advance();
            return Ok(Columns::Star);
        }
        let mut cols = Vec::new();
        loop {
            cols.push(self.consume_identifier("column name")?);
            if self.peek() != Some(&Token::Comma) {
                break;
            }
            self.advance();
        }

        Ok(Columns::List(cols))
    }

    // --- UPDATE ---

    /// Parse `table SET col = val, ... WHERE condition`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `Update`.
    fn parse_update(&mut self) -> Result<Statement> {
        self.expect(Token::Update)?;
        let table = self.consume_identifier("table name")?;
        self.expect(Token::Set)?;
        let assignments = self.parse_assignments()?;
        self.expect(Token::Where)?;
        let condition = self.parse_condition()?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::Update {
            table,
            assignments,
            condition,
        })
    }

    /// Parse a comma-separated list of `col = value` assignments.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<Assignment>>` — the assignments.
    fn parse_assignments(&mut self) -> Result<Vec<Assignment>> {
        let mut assignments = Vec::new();
        loop {
            let column = self.consume_identifier("column name")?;
            self.expect(Token::Eq)?;
            let value = self.parse_value()?;
            assignments.push(Assignment { column, value });
            if self.peek() != Some(&Token::Comma) {
                break;
            }
            self.advance();
        }

        Ok(assignments)
    }

    // --- DELETE ---

    /// Parse `FROM table WHERE condition`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `Delete`.
    fn parse_delete(&mut self) -> Result<Statement> {
        self.expect(Token::Delete)?;
        self.expect(Token::From)?;
        let table = self.consume_identifier("table name")?;
        self.expect(Token::Where)?;
        let condition = self.parse_condition()?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::Delete { table, condition })
    }

    // --- SHOW ---

    /// Parse `TABLES` or `DATABASES`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `ShowTables` or `ShowDatabases`.
    fn parse_show(&mut self) -> Result<Statement> {
        self.expect(Token::Show)?;
        match self.peek() {
            Some(Token::Tables) => {
                self.advance();
                self.expect(Token::Semicolon)?;
                Ok(Statement::ShowTables)
            }
            Some(Token::Databases) => {
                self.advance();
                self.expect(Token::Semicolon)?;
                Ok(Statement::ShowDatabases)
            }
            Some(tok) => bail!(
                "expected TABLES or DATABASES, found {:?} at {}",
                tok,
                self.pos
            ),
            None => bail!("expected TABLES or DATABASES, found EOF at {}", self.pos),
        }
    }

    // --- USE ---

    /// Parse `USE name`.
    ///
    /// # Returns
    ///
    /// * `Result<Statement>` — `UseDatabase`.
    fn parse_use(&mut self) -> Result<Statement> {
        self.expect(Token::Use)?;
        let name = self.consume_identifier("database name")?;
        self.expect(Token::Semicolon)?;

        Ok(Statement::UseDatabase { name })
    }

    // --- shared helpers ---

    /// Parse a WHERE clause boolean expression.
    ///
    /// # Returns
    ///
    /// * `Result<BoolExpr>` — the boolean expression tree.
    fn parse_condition(&mut self) -> Result<BoolExpr> {
        self.parse_or_expr()
    }

    /// Parse an OR expression (lowest precedence).
    ///
    /// # Returns
    ///
    /// * `Result<BoolExpr>` — the expression tree.
    fn parse_or_expr(&mut self) -> Result<BoolExpr> {
        let mut left = self.parse_and_expr()?;
        while self.peek() == Some(&Token::Or) {
            self.advance();
            let right = self.parse_and_expr()?;
            left = BoolExpr::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// Parse an AND expression (higher precedence than OR).
    ///
    /// # Returns
    ///
    /// * `Result<BoolExpr>` — the expression tree.
    fn parse_and_expr(&mut self) -> Result<BoolExpr> {
        let mut left = self.parse_atom()?;
        while self.peek() == Some(&Token::And) {
            self.advance();
            let right = self.parse_atom()?;
            left = BoolExpr::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// Parse an atomic boolean expression: either a parenthesized sub-expression or a comparison.
    ///
    /// # Returns
    ///
    /// * `Result<BoolExpr>` — the expression tree.
    fn parse_atom(&mut self) -> Result<BoolExpr> {
        if self.peek() == Some(&Token::LParen) {
            self.advance();
            let expr = self.parse_or_expr()?;
            self.expect(Token::RParen)?;
            Ok(expr)
        } else {
            let (column, op, value) = self.parse_comparison()?;
            Ok(BoolExpr::Comparison { column, op, value })
        }
    }

    /// Parse a comparison `column operator value`.
    ///
    /// # Returns
    ///
    /// * `Result<(String, Operator, Value)>` — the column name, operator, and value.
    fn parse_comparison(&mut self) -> Result<(String, Operator, Value)> {
        let column = self.consume_identifier("column name")?;
        let op = self.parse_operator()?;
        let value = self.parse_value()?;

        Ok((column, op, value))
    }

    /// Parse a comparison operator.
    ///
    /// # Returns
    ///
    /// * `Result<Operator>` — the operator.
    fn parse_operator(&mut self) -> Result<Operator> {
        let tok = self.advance();
        match tok {
            Token::Eq => Ok(Operator::Eq),
            Token::Ne => Ok(Operator::Ne),
            Token::Gt => Ok(Operator::Gt),
            Token::Lt => Ok(Operator::Lt),
            Token::Ge => Ok(Operator::Ge),
            Token::Le => Ok(Operator::Le),
            _ => bail!(
                "expected comparison operator, found {:?} at {}",
                tok,
                self.pos - 1
            ),
        }
    }

    /// Peek at the next token without consuming it.
    ///
    /// # Returns
    ///
    /// * `Option<&Token>` — the next token, or `None` at end of input.
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// Consume and return the next token.
    ///
    /// # Returns
    ///
    /// * `Token` — the consumed token.
    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        self.pos += 1;
        tok
    }

    /// Expect and consume a specific token, or fail.
    ///
    /// # Arguments
    ///
    /// * `expected` — the token to expect.
    ///
    /// # Returns
    ///
    /// * `Result<()>` — succeeds if the next token matches.
    fn expect(&mut self, expected: Token) -> Result<()> {
        match self.peek() {
            Some(tok) if *tok == expected => {
                self.advance();
                Ok(())
            }
            Some(tok) => bail!(
                "expected {:?}, found {:?} at position {}",
                expected,
                tok,
                self.pos
            ),
            None => bail!(
                "expected {:?}, found EOF at position {}",
                expected,
                self.pos
            ),
        }
    }

    /// Consume an identifier token and return its name.
    ///
    /// # Arguments
    ///
    /// * `what` — the expected identifier kind (e.g. `"table name"`), used in the error message.
    ///
    /// # Returns
    ///
    /// * `Result<String>` — the identifier string.
    fn consume_identifier(&mut self, what: &str) -> Result<String> {
        let tok = self.advance();
        match tok {
            Token::Identifier(s) => Ok(s),
            _ => bail!("expected {}, found {:?} at {}", what, tok, self.pos - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::lexer::Lexer;
    use super::*;

    fn parse_sql(sql: &str) -> Result<Statement> {
        let mut lexer = Lexer::new(sql);
        let tokens = lexer.tokenize()?;
        let mut parser = Parser::new(tokens);
        parser.parse()
    }

    #[test]
    fn test_create_table() {
        let stmt = parse_sql("CREATE TABLE users (id INTEGER, name TEXT, age INTEGER);").unwrap();
        match stmt {
            Statement::CreateTable { name, columns } => {
                assert_eq!(name, "users");
                assert_eq!(columns.len(), 3);
                assert_eq!(columns[0].name, "id");
                assert_eq!(columns[0].data_type, DataType::Integer);
                assert_eq!(columns[1].name, "name");
                assert_eq!(columns[1].data_type, DataType::Text);
                assert_eq!(columns[2].name, "age");
                assert_eq!(columns[2].data_type, DataType::Integer);
            }
            _ => panic!("expected CreateTable"),
        }
    }

    #[test]
    fn test_create_table_with_char() {
        let stmt = parse_sql("CREATE TABLE users (name CHAR(20), age INT);").unwrap();
        match stmt {
            Statement::CreateTable { name, columns } => {
                assert_eq!(name, "users");
                assert_eq!(columns.len(), 2);
                assert_eq!(columns[0].name, "name");
                assert_eq!(columns[0].data_type, DataType::Char(20));
                assert_eq!(columns[1].name, "age");
                assert_eq!(columns[1].data_type, DataType::Integer);
            }
            _ => panic!("expected CreateTable"),
        }
    }

    #[test]
    fn test_drop_table() {
        let stmt = parse_sql("DROP TABLE users;").unwrap();
        match stmt {
            Statement::DropTable { name } => assert_eq!(name, "users"),
            _ => panic!("expected DropTable"),
        }
    }

    #[test]
    fn test_create_database() {
        let stmt = parse_sql("CREATE DATABASE XJGL;").unwrap();
        match stmt {
            Statement::CreateDatabase { name } => assert_eq!(name, "XJGL"),
            _ => panic!("expected CreateDatabase"),
        }
    }

    #[test]
    fn test_drop_database() {
        let stmt = parse_sql("DROP DATABASE XJGL;").unwrap();
        match stmt {
            Statement::DropDatabase { name } => assert_eq!(name, "XJGL"),
            _ => panic!("expected DropDatabase"),
        }
    }

    #[test]
    fn test_show_databases() {
        let stmt = parse_sql("SHOW DATABASES;").unwrap();
        assert!(matches!(stmt, Statement::ShowDatabases));
    }

    #[test]
    fn test_use_database() {
        let stmt = parse_sql("USE XJGL;").unwrap();
        match stmt {
            Statement::UseDatabase { name } => assert_eq!(name, "XJGL"),
            _ => panic!("expected UseDatabase"),
        }
    }

    #[test]
    fn test_create_index() {
        let stmt = parse_sql("CREATE INDEX idx_age ON users (age);").unwrap();
        match stmt {
            Statement::CreateIndex {
                name,
                table,
                column,
            } => {
                assert_eq!(name, "idx_age");
                assert_eq!(table, "users");
                assert_eq!(column, "age");
            }
            _ => panic!("expected CreateIndex"),
        }
    }

    #[test]
    fn test_drop_index() {
        let stmt = parse_sql("DROP INDEX idx_age;").unwrap();
        match stmt {
            Statement::DropIndex { name } => assert_eq!(name, "idx_age"),
            _ => panic!("expected DropIndex"),
        }
    }

    #[test]
    fn test_insert() {
        let stmt = parse_sql("INSERT INTO users VALUES (1, 'alice', 18);").unwrap();
        match stmt {
            Statement::Insert {
                table,
                columns,
                values,
            } => {
                assert_eq!(table, "users");
                assert_eq!(columns, None);
                assert_eq!(values.len(), 3);
                assert_eq!(values[0], Value::Integer(1));
                assert_eq!(values[1], Value::Text("alice".to_string()));
                assert_eq!(values[2], Value::Integer(18));
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_select_star() {
        let stmt = parse_sql("SELECT * FROM users;").unwrap();
        match stmt {
            Statement::Select {
                columns,
                tables,
                condition,
            } => {
                assert_eq!(columns, Columns::Star);
                assert_eq!(tables, vec!["users"]);
                assert!(condition.is_none());
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn test_select_columns() {
        let stmt = parse_sql("SELECT id, name FROM users;").unwrap();
        match stmt {
            Statement::Select {
                columns,
                tables,
                condition,
            } => {
                assert_eq!(
                    columns,
                    Columns::List(vec!["id".to_string(), "name".to_string()])
                );
                assert_eq!(tables, vec!["users"]);
                assert!(condition.is_none());
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn test_select_with_where() {
        let stmt = parse_sql("SELECT id, name FROM users WHERE age > 18;").unwrap();
        match stmt {
            Statement::Select {
                columns,
                tables,
                condition: Some(BoolExpr::Comparison { column, op, value }),
            } => {
                assert_eq!(
                    columns,
                    Columns::List(vec!["id".to_string(), "name".to_string()])
                );
                assert_eq!(tables, vec!["users"]);
                assert_eq!(column, "age");
                assert_eq!(op, Operator::Gt);
                assert_eq!(value, Value::Integer(18));
            }
            _ => panic!("expected Select with WHERE"),
        }
    }

    #[test]
    fn test_update() {
        let stmt = parse_sql("UPDATE users SET name = 'bob' WHERE id = 1;").unwrap();
        match stmt {
            Statement::Update {
                table,
                assignments,
                condition: BoolExpr::Comparison { column, op, value },
            } => {
                assert_eq!(table, "users");
                assert_eq!(assignments.len(), 1);
                assert_eq!(assignments[0].column, "name");
                assert_eq!(assignments[0].value, Value::Text("bob".to_string()));
                assert_eq!(column, "id");
                assert_eq!(op, Operator::Eq);
                assert_eq!(value, Value::Integer(1));
            }
            _ => panic!("expected Update"),
        }
    }

    #[test]
    fn test_delete() {
        let stmt = parse_sql("DELETE FROM users WHERE id = 1;").unwrap();
        match stmt {
            Statement::Delete {
                table,
                condition: BoolExpr::Comparison { column, op, value },
            } => {
                assert_eq!(table, "users");
                assert_eq!(column, "id");
                assert_eq!(op, Operator::Eq);
                assert_eq!(value, Value::Integer(1));
            }
            _ => panic!("expected Delete"),
        }
    }

    #[test]
    fn test_error_unexpected_token() {
        let result = parse_sql("FOO bar;");
        assert!(result.is_err());
    }

    #[test]
    fn test_error_missing_semicolon() {
        let result = parse_sql("SELECT * FROM users");
        assert!(result.is_err());
    }

    #[test]
    fn test_case_insensitive() {
        let stmt = parse_sql("select * from users;").unwrap();
        match stmt {
            Statement::Select {
                columns, tables, ..
            } => {
                assert_eq!(columns, Columns::Star);
                assert_eq!(tables, vec!["users"]);
            }
            _ => panic!("expected Select"),
        }
    }

    #[test]
    fn test_boolean_and_null() {
        let stmt = parse_sql("INSERT INTO users VALUES (true, false, null);").unwrap();
        match stmt {
            Statement::Insert {
                columns, values, ..
            } => {
                assert_eq!(columns, None);
                assert!(matches!(values[0], Value::Boolean(true)));
                assert!(matches!(values[1], Value::Boolean(false)));
                assert!(matches!(values[2], Value::Null));
            }
            _ => panic!("expected Insert"),
        }
    }

    #[test]
    fn test_select_where_equals() {
        let stmt = parse_sql("SELECT id, name FROM users WHERE age = 18;").unwrap();
        assert!(matches!(
            stmt,
            Statement::Select {
                condition: Some(BoolExpr::Comparison {
                    op: Operator::Eq,
                    ..
                }),
                ..
            }
        ));
    }

    #[test]
    fn test_select_where_and() {
        let stmt = parse_sql("SELECT id FROM t WHERE a > 1 AND b = 2;").unwrap();
        assert!(matches!(
            stmt,
            Statement::Select {
                condition: Some(BoolExpr::And(_, _)),
                ..
            }
        ));
    }

    #[test]
    fn test_select_where_or() {
        let stmt = parse_sql("SELECT id FROM t WHERE a = 1 OR b < 2;").unwrap();
        assert!(matches!(
            stmt,
            Statement::Select {
                condition: Some(BoolExpr::Or(_, _)),
                ..
            }
        ));
    }

    #[test]
    fn test_where_nested_parens() {
        let stmt = parse_sql("SELECT id FROM t WHERE (a > 1) AND (b = 2 OR c < 3);").unwrap();
        match stmt {
            Statement::Select {
                condition: Some(BoolExpr::And(left, right)),
                ..
            } => {
                assert!(matches!(*left, BoolExpr::Comparison { .. }));
                assert!(matches!(*right, BoolExpr::Or(_, _)));
            }
            _ => panic!("expected And with Or right"),
        }
    }

    #[test]
    fn test_update_where_and() {
        let stmt = parse_sql("UPDATE t SET x = 1 WHERE a > 1 AND b = 2;").unwrap();
        assert!(matches!(
            stmt,
            Statement::Update {
                condition: BoolExpr::And(_, _),
                ..
            }
        ));
    }

    #[test]
    fn test_delete_where_or() {
        let stmt = parse_sql("DELETE FROM t WHERE a = 1 OR b = 2;").unwrap();
        assert!(matches!(
            stmt,
            Statement::Delete {
                condition: BoolExpr::Or(_, _),
                ..
            }
        ));
    }

    #[test]
    fn test_show_tables() {
        let stmt = parse_sql("SHOW TABLES;").unwrap();
        assert!(matches!(stmt, Statement::ShowTables));
    }

    #[test]
    fn test_insert_with_columns() {
        let stmt = parse_sql("INSERT INTO users (name, age) VALUES ('alice', 18);").unwrap();
        match stmt {
            Statement::Insert {
                table,
                columns: Some(cols),
                values,
            } => {
                assert_eq!(table, "users");
                assert_eq!(cols, vec!["name", "age"]);
                assert_eq!(
                    values,
                    vec![Value::Text("alice".into()), Value::Integer(18)]
                );
            }
            _ => panic!("expected Insert with columns"),
        }
    }

    #[test]
    fn test_select_multi_table() {
        let stmt = parse_sql("SELECT * FROM t1, t2;").unwrap();
        match stmt {
            Statement::Select {
                columns: Columns::Star,
                tables,
                condition: None,
            } => {
                assert_eq!(tables, vec!["t1", "t2"]);
            }
            _ => panic!("expected Select with two tables"),
        }
    }
}
