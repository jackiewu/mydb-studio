//! Writes query results to a file row by row, in the same formats as the
//! result grid's copy menu (`src/copy.ts`), so an export of every row looks
//! like a copy of the rows on screen.

use std::io::Write;

use serde::Deserialize;
use serde_json::Value;

use crate::session::ColumnInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Tsv,
    Csv,
    Json,
    /// One INSERT per row.
    Insert,
    /// Multi-row INSERTs of [`BATCH_ROWS`] rows each.
    InsertBatch,
    Markdown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// Column names as the first line (TSV and CSV).
    #[serde(default)]
    pub header: bool,
    /// Table name for INSERTs; defaults to the columns' table.
    #[serde(default)]
    pub table: Option<String>,
}

/// Rows per statement in [`ExportFormat::InsertBatch`], so a big export
/// stays loadable within `max_allowed_packet`.
pub const BATCH_ROWS: u64 = 1000;

// Driver type names that hold numbers and need no quotes (as in copy.ts).
const NUMERIC: &[&str] = &[
    "TINY",
    "SHORT",
    "LONG",
    "INT24",
    "LONGLONG",
    "FLOAT",
    "DOUBLE",
    "DECIMAL",
    "NEWDECIMAL",
    "YEAR",
    "BIT",
];
const INTEGER: &[&str] = &["TINY", "SHORT", "LONG", "INT24", "LONGLONG", "YEAR"];

/// Streams rows into `out` in one format.
pub struct RowWriter<W: Write> {
    out: W,
    options: ExportOptions,
    columns: Vec<ColumnInfo>,
    rows: u64,
}

impl<W: Write> RowWriter<W> {
    pub fn new(out: W, options: ExportOptions) -> Self {
        Self {
            out,
            options,
            columns: Vec::new(),
            rows: 0,
        }
    }

    pub fn begin(&mut self, columns: &[ColumnInfo]) -> std::io::Result<()> {
        self.columns = columns.to_vec();
        let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
        match self.options.format {
            ExportFormat::Tsv if self.options.header => write!(self.out, "{}", names.join("\t")),
            ExportFormat::Csv if self.options.header => {
                let cells: Vec<String> = names.iter().map(|n| csv_text(n)).collect();
                write!(self.out, "{}", cells.join(","))
            }
            ExportFormat::Json => write!(self.out, "["),
            ExportFormat::Markdown => {
                let cells: Vec<String> = names.iter().map(|n| md_text(n)).collect();
                write!(
                    self.out,
                    "| {} |\n|{}|",
                    cells.join(" | "),
                    vec!["---"; names.len()].join("|")
                )
            }
            _ => Ok(()),
        }
    }

    pub fn row(&mut self, values: &[Value]) -> std::io::Result<()> {
        let first = self.rows == 0;
        let has_header = self.options.header
            && matches!(self.options.format, ExportFormat::Tsv | ExportFormat::Csv);
        self.rows += 1;
        match self.options.format {
            ExportFormat::Tsv => {
                if !first || has_header {
                    writeln!(self.out)?;
                }
                let cells: Vec<String> = values.iter().map(tsv_cell).collect();
                write!(self.out, "{}", cells.join("\t"))
            }
            ExportFormat::Csv => {
                if !first || has_header {
                    write!(self.out, "\r\n")?;
                }
                let cells: Vec<String> = values.iter().map(csv_cell).collect();
                write!(self.out, "{}", cells.join(","))
            }
            ExportFormat::Json => {
                write!(self.out, "{}\n  {{", if first { "" } else { "," })?;
                for (i, (v, c)) in values.iter().zip(&self.columns).enumerate() {
                    write!(
                        self.out,
                        "{}\n    {}: {}",
                        if i == 0 { "" } else { "," },
                        Value::from(c.name.as_str()),
                        json_value(v, c)
                    )?;
                }
                write!(
                    self.out,
                    "{}}}",
                    if self.columns.is_empty() { "" } else { "\n  " }
                )
            }
            ExportFormat::Insert => {
                if !first {
                    writeln!(self.out)?;
                }
                write!(
                    self.out,
                    "{} VALUES {};",
                    self.insert_into(),
                    self.values(values)
                )
            }
            ExportFormat::InsertBatch => {
                let n = self.rows - 1;
                if n.is_multiple_of(BATCH_ROWS) {
                    if !first {
                        writeln!(self.out, ";")?;
                    }
                    write!(
                        self.out,
                        "{} VALUES\n  {}",
                        self.insert_into(),
                        self.values(values)
                    )
                } else {
                    write!(self.out, ",\n  {}", self.values(values))
                }
            }
            ExportFormat::Markdown => {
                let cells: Vec<String> = values
                    .iter()
                    .map(|v| match v {
                        Value::Null => "NULL".to_string(),
                        other => md_text(&text(other)),
                    })
                    .collect();
                write!(self.out, "\n| {} |", cells.join(" | "))
            }
        }
    }

    /// Closes the format and flushes. Returns the number of rows written.
    pub fn finish(mut self) -> std::io::Result<u64> {
        match self.options.format {
            ExportFormat::Json if self.rows == 0 => write!(self.out, "]")?,
            ExportFormat::Json => write!(self.out, "\n]")?,
            ExportFormat::InsertBatch if self.rows > 0 => write!(self.out, ";")?,
            _ => {}
        }
        self.out.flush()?;
        Ok(self.rows)
    }

    fn insert_into(&self) -> String {
        let table = self
            .options
            .table
            .clone()
            .filter(|t| !t.is_empty())
            .or_else(|| source_table(&self.columns))
            .unwrap_or_else(|| "table_name".to_string());
        let names: Vec<String> = self.columns.iter().map(|c| ident(&c.name)).collect();
        format!("INSERT INTO {} ({})", ident(&table), names.join(", "))
    }

    fn values(&self, values: &[Value]) -> String {
        let cells: Vec<String> = values
            .iter()
            .zip(&self.columns)
            .map(|(v, c)| sql_value(v, c))
            .collect();
        format!("({})", cells.join(", "))
    }
}

/// The table all columns come from, when they come from one.
fn source_table(columns: &[ColumnInfo]) -> Option<String> {
    let mut tables = columns.iter().map(|c| &c.table).filter(|t| !t.is_empty());
    let first = tables.next()?;
    tables.all(|t| t == first).then(|| first.clone())
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn tsv_cell(v: &Value) -> String {
    if v.is_null() {
        return "NULL".into();
    }
    // Tabs and newlines would break the grid when pasted.
    let mut out = String::new();
    let mut in_break = false;
    for c in text(v).chars() {
        if matches!(c, '\t' | '\r' | '\n') {
            if !in_break {
                out.push(' ');
            }
            in_break = true;
        } else {
            out.push(c);
            in_break = false;
        }
    }
    out
}

fn csv_cell(v: &Value) -> String {
    if v.is_null() {
        return String::new();
    }
    csv_text(&text(v))
}

fn csv_text(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn md_text(s: &str) -> String {
    s.replace('|', "\\|")
        .replace("\r\n", " ")
        .replace('\n', " ")
}

fn ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// `-?\d+(\.\d+)?(e[+-]?\d+)?`
fn is_number(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let digits = |d: &str| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit());
    let mantissa_ok = match mantissa.split_once('.') {
        Some((a, b)) => digits(a) && digits(b),
        None => digits(mantissa),
    };
    let exp_ok = exp.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    mantissa_ok && exp_ok
}

fn sql_value(v: &Value, col: &ColumnInfo) -> String {
    match v {
        Value::Null => "NULL".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if NUMERIC.contains(&col.type_name.as_str()) && is_number(s) {
                return s.clone();
            }
            // Binary cells arrive as 0x hex (see to_json in session.rs).
            let hex = s.len() > 2
                && s.starts_with("0x")
                && s[2..]
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b));
            if (col.type_name.contains("BLOB") || col.type_name.contains("STRING")) && hex {
                return s.clone();
            }
            format!(
                "'{}'",
                s.replace('\\', "\\\\")
                    .replace('\'', "''")
                    .replace('\n', "\\n")
                    .replace('\r', "\\r")
            )
        }
        other => format!("'{}'", other.to_string().replace('\'', "''")),
    }
}

/// Integers and floats as JSON numbers; DECIMAL stays a string to keep its
/// exact digits.
fn json_value(v: &Value, col: &ColumnInfo) -> Value {
    const SAFE: i64 = (1 << 53) - 1;
    let Value::String(s) = v else {
        return v.clone();
    };
    let t = col.type_name.as_str();
    if INTEGER.contains(&t)
        && let Ok(n) = s.parse::<i64>()
        && (-SAFE..=SAFE).contains(&n)
    {
        return n.into();
    }
    if (t == "FLOAT" || t == "DOUBLE")
        && let Ok(f) = s.trim().parse::<f64>()
        && f.is_finite()
    {
        return f.into();
    }
    v.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cols() -> Vec<ColumnInfo> {
        [
            ("id", "LONG"),
            ("name", "VAR_STRING"),
            ("price", "NEWDECIMAL"),
        ]
        .iter()
        .map(|(n, t)| ColumnInfo {
            name: n.to_string(),
            table: "items".into(),
            type_name: t.to_string(),
        })
        .collect()
    }

    fn export(format: ExportFormat, header: bool, rows: &[Value]) -> String {
        let mut buf = Vec::new();
        let mut w = RowWriter::new(
            &mut buf,
            ExportOptions {
                format,
                header,
                table: None,
            },
        );
        w.begin(&cols()).unwrap();
        for r in rows {
            w.row(r.as_array().unwrap()).unwrap();
        }
        assert_eq!(w.finish().unwrap(), rows.len() as u64);
        String::from_utf8(buf).unwrap()
    }

    fn rows() -> Vec<Value> {
        vec![
            json!(["1", "a,\"b\"\tc", "9.50"]),
            json!(["2", null, "-1e3"]),
        ]
    }

    #[test]
    fn text_formats_match_the_copy_menu() {
        assert_eq!(
            export(ExportFormat::Tsv, true, &rows()),
            "id\tname\tprice\n1\ta,\"b\" c\t9.50\n2\tNULL\t-1e3"
        );
        assert_eq!(
            export(ExportFormat::Tsv, false, &rows()[..1]),
            "1\ta,\"b\" c\t9.50"
        );
        assert_eq!(
            export(ExportFormat::Csv, true, &rows()),
            "id,name,price\r\n1,\"a,\"\"b\"\"\tc\",9.50\r\n2,,-1e3"
        );
        assert_eq!(
            export(ExportFormat::Markdown, false, &rows()[..1]),
            "| id | name | price |\n|---|---|---|\n| 1 | a,\"b\"\tc | 9.50 |"
        );
    }

    #[test]
    fn json_is_an_array_of_objects() {
        let out = export(ExportFormat::Json, false, &rows());
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed,
            json!([
                {"id": 1, "name": "a,\"b\"\tc", "price": "9.50"},
                {"id": 2, "name": null, "price": "-1e3"}
            ])
        );
        assert!(out.starts_with("[\n  {\n    \"id\": 1,"));
        assert_eq!(export(ExportFormat::Json, false, &[]), "[]");
    }

    #[test]
    fn inserts() {
        assert_eq!(
            export(ExportFormat::Insert, false, &rows()),
            "INSERT INTO `items` (`id`, `name`, `price`) VALUES (1, 'a,\"b\"\tc', 9.50);\n\
             INSERT INTO `items` (`id`, `name`, `price`) VALUES (2, NULL, -1e3);"
        );
        let many: Vec<Value> = (0..BATCH_ROWS + 1)
            .map(|i| json!([i.to_string(), "it's", "1"]))
            .collect();
        let out = export(ExportFormat::InsertBatch, false, &many);
        assert_eq!(out.matches("INSERT INTO").count(), 2);
        assert!(out.ends_with("VALUES\n  (1000, 'it''s', 1);"));
        assert!(out.starts_with(
            "INSERT INTO `items` (`id`, `name`, `price`) VALUES\n  (0, 'it''s', 1),\n  (1,"
        ));
        assert_eq!(export(ExportFormat::InsertBatch, false, &[]), "");
    }

    #[test]
    fn numbers() {
        for yes in ["1", "-2.5", "3e10", "4.0E-2"] {
            assert!(is_number(yes), "{yes}");
        }
        for no in ["", "-", "1.", ".5", "1e", "0x1F", "1 "] {
            assert!(!is_number(no), "{no}");
        }
    }
}
