//! Universal query language: enum AST and SQL rendering trait (layer 1)
//!
//! This crate is filled in incrementally by the Phase 1 TODO items. The first
//! landed piece is [`Value`], the universal row value type that every later
//! part of the AST (literals, bind parameters, rendered binds) is built on.

mod value;

pub use value::Value;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_exported() {
        assert_eq!(Value::Int(1).kind(), "int");
    }
}
