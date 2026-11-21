//! SQL for changes made in the data grid — UPDATE, INSERT, DELETE — with literals written
//! in each engine's syntax. Rows are found by their primary key.

use crate::{DbKind, Value};

/// What a cell was changed to: text the user typed, or NULL.
#[derive(Clone, Debug, PartialEq)]
pub enum CellEdit {
    Text(String),
    Null,
}

/// One statement of a change set, and whether it must touch exactly one row (an UPDATE or
/// DELETE that finds nothing means the row was changed elsewhere: nothing is saved).
#[derive(Clone, Debug, PartialEq)]
pub struct EditStatement {
    pub sql: String,
    pub expect_one_row: bool,
}

impl DbKind {
    /// A string literal: `'it''s'`. MySQL also reads backslashes as escapes, so they are doubled.
    pub fn text_literal(&self, s: &str) -> String {
        let s = s.replace('\'', "''");
        match self {
            DbKind::Mysql(_) => format!("'{}'", s.replace('\\', "\\\\")),
            _ => format!("'{s}'"),
        }
    }

    /// A value as SQL: numbers as numbers, text quoted, binary data as a hex literal (`NULL`
    /// when only its beginning was read).
    pub fn value_literal(&self, value: &Value) -> String {
        match value {
            Value::Null => "NULL".into(),
            Value::Int(i) => i.to_string(),
            Value::Real(r) if r.is_finite() => format!("{r:?}"),
            Value::Real(r) => self.text_literal(&r.to_string()),
            Value::Num(n) if n.parse::<f64>().is_ok_and(f64::is_finite) => n.clone(),
            Value::Num(n) => self.text_literal(n),
            Value::Text(s) => self.text_literal(s),
            Value::Blob { len, head } if *len == head.len() => {
                let hex: String = head.iter().map(|b| format!("{b:02X}")).collect();
                match self {
                    DbKind::Postgres(_) => format!("'\\x{hex}'"),
                    _ => format!("X'{hex}'"),
                }
            }
            Value::Blob { .. } => "NULL".into(),
        }
    }

    /// Typed text goes in as a string literal: every engine converts it to the column's type
    /// (`'42'` into an integer, `'2026-01-02'` into a date, `'true'` into a boolean).
    fn edit_literal(&self, edit: &CellEdit) -> String {
        match edit {
            CellEdit::Null => "NULL".into(),
            CellEdit::Text(s) => self.text_literal(s),
        }
    }

    fn where_clause(&self, key: &[(&str, &Value)]) -> String {
        key.iter()
            .map(|(column, value)| match value {
                Value::Null => format!("{} IS NULL", self.quote_ident(column)),
                v => format!("{} = {}", self.quote_ident(column), self.value_literal(v)),
            })
            .collect::<Vec<_>>()
            .join(" AND ")
    }

    pub fn update_sql(&self, table: &str, set: &[(&str, &CellEdit)], key: &[(&str, &Value)]) -> EditStatement {
        let assignments: Vec<String> = set.iter().map(|(c, e)| format!("{} = {}", self.quote_ident(c), self.edit_literal(e))).collect();
        EditStatement { sql: format!("UPDATE {table} SET {} WHERE {}", assignments.join(", "), self.where_clause(key)), expect_one_row: true }
    }

    /// Columns left out get their default.
    pub fn insert_sql(&self, table: &str, values: &[(&str, &CellEdit)]) -> EditStatement {
        let sql = if values.is_empty() {
            match self {
                DbKind::Mysql(_) => format!("INSERT INTO {table} () VALUES ()"),
                _ => format!("INSERT INTO {table} DEFAULT VALUES"),
            }
        } else {
            let columns: Vec<String> = values.iter().map(|(c, _)| self.quote_ident(c)).collect();
            let literals: Vec<String> = values.iter().map(|(_, e)| self.edit_literal(e)).collect();
            format!("INSERT INTO {table} ({}) VALUES ({})", columns.join(", "), literals.join(", "))
        };
        EditStatement { sql, expect_one_row: false }
    }

    pub fn delete_sql(&self, table: &str, key: &[(&str, &Value)]) -> EditStatement {
        EditStatement { sql: format!("DELETE FROM {table} WHERE {}", self.where_clause(key)), expect_one_row: true }
    }

    /// The statement that starts a transaction.
    pub fn begin_sql(&self) -> &'static str {
        match self {
            DbKind::Mysql(_) => "START TRANSACTION",
            _ => "BEGIN",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ServerConfig;

    #[test]
    fn statements_quote_for_each_engine() {
        let pg = DbKind::Postgres(ServerConfig::default());
        let my = DbKind::Mysql(ServerConfig::default());
        let id = Value::Int(7);
        let name = CellEdit::Text("O'Brien \\ co".into());
        assert_eq!(
            pg.update_sql("\"public\".\"users\"", &[("name", &name), ("note", &CellEdit::Null)], &[("id", &id)]).sql,
            "UPDATE \"public\".\"users\" SET \"name\" = 'O''Brien \\ co', \"note\" = NULL WHERE \"id\" = 7"
        );
        assert_eq!(my.update_sql("`users`", &[("name", &name)], &[("id", &id)]).sql, "UPDATE `users` SET `name` = 'O''Brien \\\\ co' WHERE `id` = 7");
        assert_eq!(pg.delete_sql("t", &[("a", &Value::Text("x".into())), ("b", &Value::Null)]).sql, "DELETE FROM t WHERE \"a\" = 'x' AND \"b\" IS NULL");
        assert_eq!(pg.insert_sql("t", &[]).sql, "INSERT INTO t DEFAULT VALUES");
        assert_eq!(my.insert_sql("t", &[]).sql, "INSERT INTO t () VALUES ()");
        assert_eq!(pg.insert_sql("t", &[("n", &CellEdit::Text("5".into()))]).sql, "INSERT INTO t (\"n\") VALUES ('5')");
        assert_eq!(pg.value_literal(&Value::Real(0.1)), "0.1");
        assert_eq!(pg.value_literal(&Value::Num("12.50".into())), "12.50");
        assert_eq!(pg.value_literal(&Value::blob(&[0xCA, 0xFE])), "'\\xCAFE'");
        assert_eq!(my.value_literal(&Value::blob(&[0xCA, 0xFE])), "X'CAFE'");
        assert_eq!(pg.value_literal(&Value::blob(&[0u8; 100])), "NULL");
    }
}
