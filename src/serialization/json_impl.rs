//! JSON serializer for human-readable format.

use crate::{Dictionary, DictionaryNode, MappedDictionary};
use std::io::{Read, Write};

use super::{
    extract_terms, extract_terms_with_values, extract_terms_with_values_char, DictionaryFromTerms,
    DictionaryFromTermsWithValues, DictionarySerializer, SerializationError,
};

/// JSON serializer for human-readable format.
///
/// This serializer uses JSON for easy debugging and manual inspection.
/// It's less efficient than bincode but useful for development.
pub struct JsonSerializer;

impl JsonSerializer {
    /// Serialize a [`MappedDictionary`] preserving each term's value.
    ///
    /// Wire format is a JSON array of `[term, value]` pairs — incompatible
    /// with [`Self::serialize`]'s `[term, ...]` format.
    pub fn serialize_with_values<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: MappedDictionary,
        D::Node: DictionaryNode<Unit = u8>,
        D::Value: serde::Serialize,
        W: Write,
    {
        let entries = extract_terms_with_values(dict);
        serde_json::to_writer_pretty(&mut writer, &entries)?;
        Ok(())
    }

    /// Deserialize a [`MappedDictionary`] preserving each term's value.
    pub fn deserialize_with_values<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTermsWithValues,
        D::Value: serde::de::DeserializeOwned,
        R: Read,
    {
        let entries: Vec<(String, D::Value)> = serde_json::from_reader(&mut reader)?;
        Ok(D::from_terms_with_values(entries))
    }

    /// `serialize_with_values` for `Unit = char` (Unicode) backends.
    pub fn serialize_with_values_char<D, W>(
        dict: &D,
        mut writer: W,
    ) -> Result<(), SerializationError>
    where
        D: MappedDictionary,
        D::Node: DictionaryNode<Unit = char>,
        D::Value: serde::Serialize,
        W: Write,
    {
        let entries = extract_terms_with_values_char(dict);
        serde_json::to_writer_pretty(&mut writer, &entries)?;
        Ok(())
    }
}

impl DictionarySerializer for JsonSerializer {
    fn serialize<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
        W: Write,
    {
        let terms = extract_terms(dict);
        serde_json::to_writer_pretty(&mut writer, &terms)?;
        Ok(())
    }

    fn deserialize<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTerms,
        R: Read,
    {
        let terms: Vec<String> = serde_json::from_reader(&mut reader)?;
        Ok(D::from_terms(terms))
    }
}
