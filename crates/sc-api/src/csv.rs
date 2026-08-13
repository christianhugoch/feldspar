//! A table's rows in and out as CSV (design §13.1).
//!
//! The one bulk shape an admin already has a tool for: a spreadsheet exports a
//! CSV, and a table's rows are a CSV. This module is the translation on both
//! sides of that, and **nothing else** — an import goes row by row through
//! [`rows::create_row_ctx`], the same function the row endpoints and an
//! application's REST provider write through, so a CSV write is held to every
//! rule an ordinary write is: type coercion, rich-type attributes, `File` field
//! paths, ownership, RLS, and the insert triggers that fire afterwards. A bulk
//! loader that went straight to `INSERT` would be a second write path with none
//! of that, which is the thing this codebase does not do.
//!
//! **Import is not a transaction.** Each row stands or falls on its own and a
//! failure is reported with its line number rather than discarding the rows that
//! did work — a 5000-line export with three bad dates in it is a file to fix
//! three lines of, not a file to be refused whole. The caller sees both numbers.
//!
//! The wire shape is a **string**, not bytes: CSV is text, the endpoint model is
//! JSON (see [`crate::endpoint`]), and a table small enough for an admin to
//! round-trip through a spreadsheet is small enough to cross as one.

use sc_catalog::{CallerContext, Catalog, Table};
use sc_error::{Error, Result};
use sc_types::BasicType;
use serde_json::{Map, Value as Json};

use crate::rows;

/// What an import did: the rows that went in, and the ones that did not with
/// the reason and the line each was on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportOutcome {
    /// How many rows were inserted.
    pub inserted: usize,
    /// One message per rejected row, each naming its line in the file.
    pub errors: Vec<String>,
}

/// Every row of `table` as a CSV document: a header line of column names, then
/// one line per row.
///
/// The columns are the table's **stored** fields in declaration order.
/// Calculated fields are left out on purpose: they are computed on read and
/// refused on write, so putting them in the export would produce a file that
/// cannot be imported back — and a round trip is what an export is for.
pub async fn export_table(
    catalog: &Catalog,
    table: &Table,
    context: Option<&CallerContext>,
) -> Result<String> {
    let columns: Vec<&str> = stored_columns(table);
    let rows = rows::list_rows_ctx(catalog, table, context).await?;
    let Json::Array(rows) = rows else {
        return Err(Error::msg("the row layer did not return an array"));
    };

    let mut writer = ::csv::Writer::from_writer(Vec::new());
    writer
        .write_record(&columns)
        .map_err(|e| Error::msg(format!("could not write the CSV header: {e}")))?;
    for row in &rows {
        let record: Vec<String> = columns.iter().map(|c| cell(row.get(*c))).collect();
        writer
            .write_record(&record)
            .map_err(|e| Error::msg(format!("could not write a CSV row: {e}")))?;
    }
    let bytes = writer
        .into_inner()
        .map_err(|e| Error::msg(format!("could not finish the CSV: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| Error::msg(format!("the CSV was not valid UTF-8: {e}")))
}

/// Insert the rows of a CSV document into `table`, one write at a time.
///
/// The header names the columns, which must all be fields of the table — a
/// header naming something else is refused **before** any row is written, since
/// that is a wrong-file mistake rather than a bad-row one, and importing the
/// half of it that happens to match would be the unhelpful answer.
///
/// A blank cell is `null`, not the empty string: a spreadsheet has no way to
/// write "absent" other than by leaving the cell empty, and a `NOT NULL` column
/// will say so itself. The one exception is a text column, where the empty
/// string is a value a user may well have meant.
pub async fn import_table(
    catalog: &Catalog,
    table: &Table,
    document: &str,
    context: Option<&CallerContext>,
) -> Result<ImportOutcome> {
    let mut reader = ::csv::ReaderBuilder::new()
        .flexible(false)
        .from_reader(document.as_bytes());
    let header: Vec<String> = reader
        .headers()
        .map_err(|e| Error::invalid(format!("could not read the CSV header: {e}")))?
        .iter()
        .map(|h| h.trim().to_owned())
        .collect();
    if header.is_empty() {
        return Err(Error::invalid("the CSV has no header row"));
    }
    for name in &header {
        let Some(field) = table.field(name) else {
            return Err(Error::invalid(format!(
                "`{}` has no field `{name}`",
                table.name
            )));
        };
        if field.is_calc() {
            return Err(Error::invalid(format!(
                "`{name}` is a calculated field and cannot be written"
            )));
        }
    }

    let mut outcome = ImportOutcome::default();
    for (index, record) in reader.records().enumerate() {
        // Line numbers as a spreadsheet counts them: the header is line 1, so
        // the first data row is line 2. An error an admin cannot locate in the
        // file is an error they cannot fix.
        let line = index + 2;
        let record = match record {
            Ok(record) => record,
            Err(e) => {
                outcome.errors.push(format!("line {line}: {e}"));
                continue;
            }
        };
        let mut body = Map::new();
        for (name, raw) in header.iter().zip(record.iter()) {
            body.insert(name.clone(), field_json(table, name, raw));
        }
        match rows::create_row_ctx(catalog, table, &Json::Object(body), context).await {
            Ok(_) => outcome.inserted += 1,
            Err(e) => outcome.errors.push(format!("line {line}: {e}")),
        }
    }
    Ok(outcome)
}

/// The table's stored fields in declaration order — everything an import may
/// write, which is what an export should contain.
fn stored_columns(table: &Table) -> Vec<&str> {
    table
        .fields
        .iter()
        .filter(|f| !f.is_calc())
        .map(|f| f.base.name.as_str())
        .collect()
}

/// One JSON value as a CSV cell.
///
/// A string goes out as itself rather than as its JSON spelling — a quoted,
/// escaped `"hello"` in a spreadsheet cell is not what anyone wants — and a
/// null is the empty cell [`field_json`] reads back as null. Everything else
/// (numbers, booleans, and a `json`/`jsonb` column's object) takes its JSON
/// form, which is the one spelling that survives the round trip.
fn cell(value: Option<&Json>) -> String {
    match value {
        None | Some(Json::Null) => String::new(),
        Some(Json::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// A raw CSV cell as the JSON the row layer should coerce for this column.
///
/// Nearly everything is handed over as a **string** and coerced by the field's
/// own type ([`sc_types::json_to_value`] parses text into ints, floats,
/// decimals, dates, times, timestamps and UUIDs), so the CSV path inherits the
/// same coercion rules as every other write instead of restating them. The two
/// exceptions are the types where a string is not coercible and CSV has no
/// other way to spell the value: a boolean and a JSON column.
fn field_json(table: &Table, column: &str, raw: &str) -> Json {
    let basic = table
        .field(column)
        .and_then(|f| f.base.type_.as_basic().cloned());
    let trimmed = raw.trim();
    match basic {
        // The empty cell is absence — except for text, where it is the empty
        // string a user may have typed on purpose.
        _ if raw.is_empty() => match basic {
            Some(BasicType::Text) => Json::String(String::new()),
            _ => Json::Null,
        },
        Some(BasicType::Bool) => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" | "1" => Json::Bool(true),
            "false" | "f" | "no" | "n" | "0" => Json::Bool(false),
            // Not a boolean this understands: hand the text on so the row
            // layer refuses it by name, on its line, like any other bad cell.
            _ => Json::String(raw.to_owned()),
        },
        Some(BasicType::Json) => {
            serde_json::from_str(trimmed).unwrap_or_else(|_| Json::String(raw.to_owned()))
        }
        _ => Json::String(raw.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphql::testing::{id_field, table_of, typed_field};

    fn table() -> Table {
        table_of(
            "books",
            vec![
                id_field(),
                typed_field("title", BasicType::Text),
                typed_field("in_print", BasicType::Bool),
                typed_field("meta", BasicType::Json),
            ],
        )
    }

    #[test]
    fn the_columns_are_the_stored_fields_and_never_a_calculated_one() {
        let mut t = table();
        let mut calc = typed_field("doubled", BasicType::Int);
        calc.kind = sc_catalog::DataFieldKind::Calc {
            expression: "id * 2".to_owned(),
        };
        t.fields.push(calc);
        // A calc field is refused on write, so an export carrying it would
        // produce a file that could not be imported back.
        assert_eq!(stored_columns(&t), vec!["id", "title", "in_print", "meta"]);
    }

    #[test]
    fn a_cell_is_the_string_itself_and_the_json_spelling_of_anything_else() {
        assert_eq!(cell(None), "");
        assert_eq!(cell(Some(&Json::Null)), "");
        assert_eq!(cell(Some(&Json::String("a, b".to_owned()))), "a, b");
        assert_eq!(cell(Some(&serde_json::json!(3))), "3");
        assert_eq!(cell(Some(&serde_json::json!(true))), "true");
        assert_eq!(cell(Some(&serde_json::json!({"a": 1}))), r#"{"a":1}"#);
    }

    #[test]
    fn a_blank_cell_is_null_except_in_a_text_column() {
        let t = table();
        assert_eq!(field_json(&t, "id", ""), Json::Null);
        assert_eq!(field_json(&t, "title", ""), Json::String(String::new()));
    }

    #[test]
    fn booleans_and_json_are_parsed_and_everything_else_is_left_as_text() {
        let t = table();
        assert_eq!(field_json(&t, "in_print", "yes"), Json::Bool(true));
        assert_eq!(field_json(&t, "in_print", "FALSE"), Json::Bool(false));
        // Not a boolean: handed on as text so the row layer names the field.
        assert_eq!(
            field_json(&t, "in_print", "perhaps"),
            Json::String("perhaps".to_owned())
        );
        assert_eq!(
            field_json(&t, "meta", r#"{"a": 1}"#),
            serde_json::json!({"a": 1})
        );
        // An int stays text; `json_to_value` parses it against the column.
        assert_eq!(field_json(&t, "id", "7"), Json::String("7".to_owned()));
    }
}
