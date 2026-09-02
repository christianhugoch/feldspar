//! JSON ↔ Python, in both directions, and the rules for what does not convert.
//!
//! Inbound (JSON → Python) has no failures in it: an object is a `dict`, an
//! array a `list`, `null` is `None`, and a date is the ISO string the row layer
//! put on the wire — the same values a JavaScript body sees, because they are
//! the same JSON.
//!
//! Outbound is where the rules are. Beyond what JSON has, five Python types
//! convert because a body that computes with them and returns the result should
//! not have to remember to stringify it: `datetime`, `date` and `time` become
//! ISO strings, `Decimal` a number, `UUID` a string. **Anything else is an
//! error that names the type and the path to it** — a `set` in a workflow's
//! context is a bug in the body, and turning it into `null` would be that bug
//! discovered three screens later by somebody else.

use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyList, PyString, PyTuple, PyType};
use serde_json::{Map, Value as Json};

/// How deep a returned value may nest before the conversion refuses it.
///
/// A bound rather than a stack overflow: a `list` that contains itself is a
/// legal Python value and an infinite JSON one, and the difference between a
/// named error and a segfault is the whole of principle 5.
const MAX_DEPTH: usize = 64;

/// The five types outbound conversion knows beyond JSON's own, looked up once.
///
/// Held as type objects rather than checked by name: `type(x).__name__ ==
/// "Decimal"` is true of anybody's `Decimal`, and `isinstance` is what Python
/// itself means by the question.
struct Extras {
    datetime: Py<PyType>,
    date: Py<PyType>,
    time: Py<PyType>,
    decimal: Py<PyType>,
    uuid: Py<PyType>,
}

static EXTRAS: PyOnceLock<Extras> = PyOnceLock::new();

/// Import the five, at interpreter start. Costs three imports the interpreter
/// makes anyway (`datetime`, `decimal`, `uuid`) and takes the cost off the first
/// run's clock.
pub(crate) fn init(py: Python<'_>) -> PyResult<()> {
    let datetime = py.import("datetime")?;
    let decimal = py.import("decimal")?;
    let uuid = py.import("uuid")?;
    let extras = Extras {
        datetime: datetime
            .getattr("datetime")?
            .cast_into::<PyType>()?
            .unbind(),
        date: datetime.getattr("date")?.cast_into::<PyType>()?.unbind(),
        time: datetime.getattr("time")?.cast_into::<PyType>()?.unbind(),
        decimal: decimal.getattr("Decimal")?.cast_into::<PyType>()?.unbind(),
        uuid: uuid.getattr("UUID")?.cast_into::<PyType>()?.unbind(),
    };
    let _ = EXTRAS.set(py, extras);
    Ok(())
}

/// One JSON value, as Python.
pub(crate) fn from_json<'py>(py: Python<'py>, value: &Json) -> PyResult<Bound<'py, PyAny>> {
    Ok(match value {
        Json::Null => py.None().into_bound(py),
        Json::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any()
            } else if let Some(u) = n.as_u64() {
                u.into_pyobject(py)?.into_any()
            } else {
                // Every remaining JSON number is a float, and a float that came
                // out of a parser is finite.
                n.as_f64().unwrap_or(f64::NAN).into_pyobject(py)?.into_any()
            }
        }
        Json::String(s) => PyString::new(py, s).into_any(),
        Json::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(from_json(py, item)?)?;
            }
            list.into_any()
        }
        Json::Object(fields) => {
            let dict = PyDict::new(py);
            for (key, item) in fields {
                dict.set_item(key, from_json(py, item)?)?;
            }
            dict.into_any()
        }
    })
}

/// One Python value, as JSON, or the sentence saying why it is not.
///
/// `root` is what the path in that sentence starts with — `result` for what a
/// body returned, `the plan` for what it handed a host surface — so the message
/// names a place the author can look at rather than a type in isolation.
pub(crate) fn to_json(value: &Bound<'_, PyAny>, root: &str) -> Result<Json, String> {
    let mut path = root.to_owned();
    convert(value, &mut path, 0)
}

fn convert(value: &Bound<'_, PyAny>, path: &mut String, depth: usize) -> Result<Json, String> {
    if depth > MAX_DEPTH {
        return Err(format!(
            "`{path}` nests more than {MAX_DEPTH} deep; a value that contains \
             itself has no JSON form"
        ));
    }
    let py = value.py();
    if value.is_none() {
        return Ok(Json::Null);
    }
    // Before the integer branch, because in Python a `bool` **is** an `int` and
    // `True` must not come back as `1`.
    if let Ok(b) = value.cast::<PyBool>() {
        return Ok(Json::Bool(b.is_true()));
    }
    if let Ok(s) = value.cast::<PyString>() {
        return Ok(Json::String(s.to_string_lossy().into_owned()));
    }
    if value.is_instance_of::<pyo3::types::PyInt>() {
        if let Ok(i) = value.extract::<i64>() {
            return Ok(Json::from(i));
        }
        if let Ok(u) = value.extract::<u64>() {
            return Ok(Json::from(u));
        }
        // Python's integers are unbounded and JSON's are not. Refusing names the
        // value; converting to a float would round it silently.
        return Err(format!(
            "`{path}` is an integer too large for JSON ({})",
            value
                .str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
    }
    if value.is_instance_of::<pyo3::types::PyFloat>() {
        let f = value.extract::<f64>().unwrap_or(f64::NAN);
        return match serde_json::Number::from_f64(f) {
            Some(n) => Ok(Json::Number(n)),
            None => Err(format!(
                "`{path}` is `{f}`, and JSON has no way to express it"
            )),
        };
    }
    if let Ok(dict) = value.cast::<PyDict>() {
        let mut fields = Map::new();
        for (key, item) in dict.iter() {
            let Ok(name) = key.cast::<PyString>() else {
                return Err(format!(
                    "`{path}` has a key of type `{}`; a JSON object's keys are strings",
                    type_name(&key)
                ));
            };
            let name = name.to_string_lossy().into_owned();
            let len = path.len();
            path.push_str(&format!("[{name:?}]"));
            let item = convert(&item, path, depth + 1)?;
            path.truncate(len);
            fields.insert(name, item);
        }
        return Ok(Json::Object(fields));
    }
    if value.cast::<PyList>().is_ok() || value.cast::<PyTuple>().is_ok() {
        let mut items = Vec::new();
        for (index, item) in value.try_iter().map_err(|e| e.to_string())?.enumerate() {
            let item = item.map_err(|e| e.to_string())?;
            let len = path.len();
            path.push_str(&format!("[{index}]"));
            items.push(convert(&item, path, depth + 1)?);
            path.truncate(len);
        }
        return Ok(Json::Array(items));
    }
    // The five that are not JSON's but are an app builder's.
    if let Some(extras) = EXTRAS.get(py) {
        // `datetime` before `date`, because it is a subclass of it and
        // `isinstance` would answer yes to the wrong one first.
        for candidate in [&extras.datetime, &extras.date, &extras.time] {
            if value.is_instance(candidate.bind(py)).unwrap_or(false) {
                return match value
                    .call_method0("isoformat")
                    .and_then(|s| s.extract::<String>())
                {
                    Ok(s) => Ok(Json::String(s)),
                    Err(e) => Err(format!("`{path}` could not be rendered as a date: {e}")),
                };
            }
        }
        if value.is_instance(extras.decimal.bind(py)).unwrap_or(false) {
            let f = value.extract::<f64>().unwrap_or(f64::NAN);
            return match serde_json::Number::from_f64(f) {
                Some(n) => Ok(Json::Number(n)),
                None => Err(format!("`{path}` is a Decimal (`{f}`) with no JSON form")),
            };
        }
        if value.is_instance(extras.uuid.bind(py)).unwrap_or(false) {
            return Ok(Json::String(
                value
                    .str()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ));
        }
    }
    Err(format!(
        "`{path}` is a `{}`, which has no JSON form",
        type_name(value)
    ))
}

fn type_name(value: &Bound<'_, PyAny>) -> String {
    value
        .get_type()
        .name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "?".to_owned())
}
