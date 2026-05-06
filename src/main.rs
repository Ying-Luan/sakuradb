//! Binary entrypoint — reads SQL statements from a file and executes them.
//!
//! Usage: `sakuradb <sql_file>`

use std::{env, fs, process};

use sakuradb::Database;

/// Read SQL statements from a file, execute each non-empty line, and print results.
fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: sakuradb <sql_file>");
        process::exit(1);
    });

    let sql = fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("Error reading file: {}", e);
        process::exit(1);
    });

    let mut db = Database::new("data");

    let cleaned: String = sql
        .lines()
        .map(|line| match line.find("//") {
            Some(pos) => line[..pos].trim().to_string(),
            None => line.trim().to_string(),
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    for part in cleaned.split(';') {
        let stmt = part.trim();
        if stmt.is_empty() {
            continue;
        }
        if stmt.to_uppercase().trim_end_matches(';') == "EXIT" {
            return;
        }
        println!("> {};", stmt);
        match db.execute(&format!("{};", stmt)) {
            Ok(output) => println!("{}\n", output),
            Err(e) => eprintln!("Error: {}\n", e),
        }
    }
}
