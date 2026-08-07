//! Tiny process-global string interner for `&'static str` fields that
//! must survive serde round-trips (save/load). Added in this fork for
//! world persistence.
//!
//! Several persisted structs carry i18n reason keys typed `&'static str`
//! — a small closed set of literals. Deserialize can't produce a
//! `&'static str` from owned input, so `static_str_serde` serializes the
//! key as a plain string and re-interns it on load. Interning leaks each
//! *distinct* string once per process (bounded by the key set), never
//! per occurrence.

use std::collections::HashSet;
use std::sync::Mutex;

static INTERNED: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);

/// Return a `&'static str` equal to `s`, leaking at most once per
/// distinct string for the process lifetime.
pub fn intern_static(s: String) -> &'static str {
    let mut guard = INTERNED.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if let Some(existing) = set.get(s.as_str()) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.into_boxed_str());
    set.insert(leaked);
    leaked
}

/// Serde adapter for `&'static str` fields on persisted types:
/// `#[serde(with = "crate::utils::intern::static_str_serde")]`.
pub mod static_str_serde {
    pub fn serialize<S: serde::Serializer>(v: &str, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(v)
    }

    pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
        let s: String = serde::Deserialize::deserialize(d)?;
        Ok(super::intern_static(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_pointer_for_equal_strings() {
        let a = intern_static("dec_test_key_x".to_string());
        let b = intern_static("dec_test_key_x".to_string());
        assert_eq!(a, b);
        assert!(std::ptr::eq(a, b), "equal strings must intern to one leak");
    }
}
