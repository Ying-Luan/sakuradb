//! A simple lexer.
//!
//! It tokenizes SQL input strings into a sequence of tokens that can be consumed by the parser.

use anyhow::{Result, bail};

/// Tokens produced by the lexer.
#[derive(Debug, PartialEq, Clone)]
pub(crate) enum Token {
    /// SELECT keyword.
    Select,
    /// FROM keyword.
    From,
    /// WHERE keyword.
    Where,
    /// INSERT keyword.
    Insert,
    /// INTO keyword.
    Into,
    /// VALUES keyword.
    Values,
    /// UPDATE keyword.
    Update,
    /// SET keyword.
    Set,
    /// DELETE keyword.
    Delete,
    /// CREATE keyword.
    Create,
    /// DROP keyword.
    Drop,
    /// TABLE keyword.
    Table,
    /// INDEX keyword.
    Index,
    /// ON keyword.
    On,
    /// TRUE keyword.
    True,
    /// FALSE keyword.
    False,
    /// NULL keyword.
    Null,
    /// AND keyword.
    And,
    /// OR keyword.
    Or,
    /// SHOW keyword.
    Show,
    /// TABLES keyword.
    Tables,
    /// DATABASE keyword.
    Database,
    /// DATABASES keyword.
    Databases,
    /// USE keyword.
    Use,
    // --- Data types ---
    /// INTEGER type.
    Integer,
    /// FLOAT type.
    Float,
    /// TEXT type.
    Text,
    /// BOOLEAN type.
    Boolean,
    /// CHAR type.
    Char,
    // --- Operators and punctuation ---
    /// = operator.
    Eq,
    /// != operator.
    Ne,
    /// > operator.
    Gt,
    /// < operator.
    Lt,
    /// >= operator.
    Ge,
    /// <= operator.
    Le,
    /// ,
    Comma,
    /// (
    LParen,
    /// )
    RParen,
    /// *
    Star,
    /// ;
    Semicolon,
    // --- Literals and identifiers ---
    /// Integer literal.
    IntLiteral(i64),
    /// Float literal.
    FloatLiteral(f64),
    /// String literal.
    StringLiteral(String),
    /// Unquoted identifier.
    Identifier(String),
}

/// A simple lexer for SQL input strings.
pub(crate) struct Lexer {
    /// The characters of the input string for easy indexing.
    chars: Vec<char>,
    /// The current position in the input string.
    pos: usize,
}

impl Lexer {
    /// Create a new lexer for the given SQL input.
    ///
    /// # Arguments
    ///
    /// * `input` - The SQL query string to tokenize.
    ///
    /// # Returns
    ///
    /// * `Lexer` - A new lexer instance ready to tokenize the input.
    pub(crate) fn new(input: &str) -> Self {
        Self {
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    /// Tokenize the entire input into a sequence of tokens.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<Token>>` — the token sequence on success.
    pub(crate) fn tokenize(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            if self.pos >= self.chars.len() {
                break;
            }
            tokens.push(self.next_token()?);
        }
        Ok(tokens)
    }

    /// Skip whitespace characters from the current position.
    ///
    /// Advances `pos` past any whitespace characters.
    fn skip_whitespace(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    /// Read the next token from the current position.
    ///
    /// # Returns
    ///
    /// * `Result<Token>` — the next token on success.
    fn next_token(&mut self) -> Result<Token> {
        let ch = self.chars[self.pos];

        if ch.is_ascii_alphabetic() || ch == '_' {
            return self.lex_word();
        }

        if ch.is_ascii_digit() {
            return self.lex_number();
        }

        if ch == '\'' {
            return self.lex_string();
        }

        match ch {
            '=' => {
                self.pos += 1;
                Ok(Token::Eq)
            }
            '!' => {
                self.pos += 1;
                if self.pos < self.chars.len() && self.chars[self.pos] == '=' {
                    self.pos += 1;
                    Ok(Token::Ne)
                } else {
                    bail!("unexpected character '!' at position {}", self.pos - 1);
                }
            }
            '>' => {
                self.pos += 1;
                if self.pos < self.chars.len() && self.chars[self.pos] == '=' {
                    self.pos += 1;
                    Ok(Token::Ge)
                } else {
                    Ok(Token::Gt)
                }
            }
            '<' => {
                self.pos += 1;
                if self.pos < self.chars.len() && self.chars[self.pos] == '=' {
                    self.pos += 1;
                    Ok(Token::Le)
                } else {
                    Ok(Token::Lt)
                }
            }
            ',' => {
                self.pos += 1;
                Ok(Token::Comma)
            }
            '(' => {
                self.pos += 1;
                Ok(Token::LParen)
            }
            ')' => {
                self.pos += 1;
                Ok(Token::RParen)
            }
            '*' => {
                self.pos += 1;
                Ok(Token::Star)
            }
            ';' => {
                self.pos += 1;
                Ok(Token::Semicolon)
            }
            _ => bail!("unexpected character '{}' at position {}", ch, self.pos),
        }
    }

    /// Lex an identifier or keyword.
    ///
    /// # Returns
    ///
    /// * `Result<Token>` — a keyword token or an `Identifier`.
    fn lex_word(&mut self) -> Result<Token> {
        let start = self.pos;
        while self.pos < self.chars.len()
            && (self.chars[self.pos].is_ascii_alphanumeric() || self.chars[self.pos] == '_')
        {
            self.pos += 1;
        }
        let word: String = self.chars[start..self.pos].iter().collect();
        let upper = word.to_uppercase();

        match upper.as_str() {
            "SELECT" => Ok(Token::Select),
            "FROM" => Ok(Token::From),
            "WHERE" => Ok(Token::Where),
            "INSERT" => Ok(Token::Insert),
            "INTO" => Ok(Token::Into),
            "VALUES" => Ok(Token::Values),
            "UPDATE" => Ok(Token::Update),
            "SET" => Ok(Token::Set),
            "DELETE" => Ok(Token::Delete),
            "CREATE" => Ok(Token::Create),
            "DROP" => Ok(Token::Drop),
            "TABLE" => Ok(Token::Table),
            "INDEX" => Ok(Token::Index),
            "ON" => Ok(Token::On),
            "TRUE" => Ok(Token::True),
            "FALSE" => Ok(Token::False),
            "NULL" => Ok(Token::Null),
            "AND" => Ok(Token::And),
            "OR" => Ok(Token::Or),
            "SHOW" => Ok(Token::Show),
            "TABLES" => Ok(Token::Tables),
            "DATABASE" => Ok(Token::Database),
            "DATABASES" => Ok(Token::Databases),
            "USE" => Ok(Token::Use),
            "INTEGER" => Ok(Token::Integer),
            "FLOAT" => Ok(Token::Float),
            "TEXT" => Ok(Token::Text),
            "BOOLEAN" => Ok(Token::Boolean),
            "CHAR" => Ok(Token::Char),
            "INT" => Ok(Token::Integer),
            _ => Ok(Token::Identifier(word)),
        }
    }

    /// Lex an integer or float literal.
    ///
    /// # Returns
    ///
    /// * `Result<Token>` — `IntLiteral` or `FloatLiteral`.
    fn lex_number(&mut self) -> Result<Token> {
        let start = self.pos;
        while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let has_dot = self.pos < self.chars.len() && self.chars[self.pos] == '.';
        if has_dot {
            let next_pos = self.pos + 1;
            if next_pos < self.chars.len() && self.chars[next_pos].is_ascii_digit() {
                self.pos += 1;
                while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_digit() {
                    self.pos += 1;
                }
                let num: String = self.chars[start..self.pos].iter().collect();
                return Ok(Token::FloatLiteral(num.parse::<f64>().unwrap()));
            }
        }
        let num: String = self.chars[start..self.pos].iter().collect();
        Ok(Token::IntLiteral(num.parse::<i64>().unwrap()))
    }

    /// Lex a single-quoted string literal.
    ///
    /// # Returns
    ///
    /// * `Result<Token>` — `StringLiteral` with the inner content.
    fn lex_string(&mut self) -> Result<Token> {
        self.pos += 1; // skip opening quote
        let start = self.pos;
        while self.pos < self.chars.len() && self.chars[self.pos] != '\'' {
            self.pos += 1;
        }
        if self.pos >= self.chars.len() {
            bail!("unterminated string literal");
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        self.pos += 1; // skip closing quote
        Ok(Token::StringLiteral(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenize(input: &str) -> Result<Vec<Token>> {
        let mut lexer = Lexer::new(input);
        lexer.tokenize()
    }

    #[test]
    fn test_keywords() {
        let tokens = tokenize(
            "SELECT FROM WHERE INSERT INTO VALUES UPDATE SET DELETE CREATE DROP TABLE INDEX ON",
        )
        .unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Select,
                Token::From,
                Token::Where,
                Token::Insert,
                Token::Into,
                Token::Values,
                Token::Update,
                Token::Set,
                Token::Delete,
                Token::Create,
                Token::Drop,
                Token::Table,
                Token::Index,
                Token::On,
            ]
        );
    }

    #[test]
    fn test_data_types() {
        let tokens = tokenize("INTEGER FLOAT TEXT BOOLEAN").unwrap();
        assert_eq!(
            tokens,
            vec![Token::Integer, Token::Float, Token::Text, Token::Boolean,]
        );
    }

    #[test]
    fn test_bool_and_null() {
        let tokens = tokenize("TRUE FALSE NULL").unwrap();
        assert_eq!(tokens, vec![Token::True, Token::False, Token::Null,]);
    }

    #[test]
    fn test_identifiers() {
        let tokens = tokenize("users id name age _private hello123").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Identifier("users".into()),
                Token::Identifier("id".into()),
                Token::Identifier("name".into()),
                Token::Identifier("age".into()),
                Token::Identifier("_private".into()),
                Token::Identifier("hello123".into()),
            ]
        );
    }

    #[test]
    fn test_numbers() {
        let tokens = tokenize("42 0 100 3.14 0.5").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::IntLiteral(42),
                Token::IntLiteral(0),
                Token::IntLiteral(100),
                Token::FloatLiteral(3.14),
                Token::FloatLiteral(0.5),
            ]
        );
    }

    #[test]
    fn test_strings() {
        let tokens = tokenize("'hello' '' 'hello world'").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::StringLiteral("hello".into()),
                Token::StringLiteral("".into()),
                Token::StringLiteral("hello world".into()),
            ]
        );
    }

    #[test]
    fn test_operators() {
        let tokens = tokenize("= != > < >= <=").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Eq,
                Token::Ne,
                Token::Gt,
                Token::Lt,
                Token::Ge,
                Token::Le,
            ]
        );
    }

    #[test]
    fn test_punctuation() {
        let tokens = tokenize(", ( ) * ;").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Comma,
                Token::LParen,
                Token::RParen,
                Token::Star,
                Token::Semicolon,
            ]
        );
    }

    #[test]
    fn test_whitespace() {
        let a = tokenize("SELECT 1").unwrap();
        let b = tokenize("SELECT\t\t1").unwrap();
        let c = tokenize("SELECT\n\n1").unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn test_error_bare_exclamation() {
        assert!(tokenize("SELECT !").is_err());
    }

    #[test]
    fn test_error_unterminated_string() {
        assert!(tokenize("'hello").is_err());
    }

    #[test]
    fn test_error_unexpected_char() {
        assert!(tokenize("SELECT @").is_err());
    }
}
