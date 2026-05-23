//! Bincode serializer for compact binary format.

use crate::{Dictionary, DictionaryNode, MappedDictionary};
use std::io::{Read, Write};

use super::{
    extract_terms, extract_terms_with_values, extract_terms_with_values_char, DictionaryFromTerms,
    DictionaryFromTermsWithValues, DictionarySerializer, SerializationError,
};

use crate::suffix_automaton::SuffixAutomaton;

/// Bincode serializer for compact binary format.
///
/// This serializer uses bincode for fast, space-efficient serialization.
/// It's ideal for production use where storage space and load time matter.
pub struct BincodeSerializer;

impl BincodeSerializer {
    /// Serialize a suffix automaton using its source texts.
    ///
    /// This is more efficient than using the generic `serialize()` method,
    /// which would extract all substrings.
    pub fn serialize_suffix_automaton<W>(
        automaton: &SuffixAutomaton,
        mut writer: W,
    ) -> Result<(), SerializationError>
    where
        W: Write,
    {
        let texts = automaton.source_texts();
        crate::serialization::bincode_compat::serialize_into(&mut writer, &texts)?;
        Ok(())
    }

    /// Deserialize a suffix automaton from source texts.
    pub fn deserialize_suffix_automaton<R>(
        mut reader: R,
    ) -> Result<SuffixAutomaton, SerializationError>
    where
        R: Read,
    {
        let texts: Vec<String> =
            crate::serialization::bincode_compat::deserialize_from(&mut reader)?;
        Ok(SuffixAutomaton::from_texts(texts))
    }

    /// Serialize a [`MappedDictionary`] preserving each term's value.
    ///
    /// Use this instead of [`Self::serialize`] when the dictionary's value
    /// type is non-`()`. The wire format is a `Vec<(String, V)>` — strictly
    /// different from [`Self::serialize`]'s `Vec<String>`, so files written
    /// by one method cannot be read by the other.
    pub fn serialize_with_values<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: MappedDictionary,
        D::Node: DictionaryNode<Unit = u8>,
        D::Value: serde::Serialize,
        W: Write,
    {
        let entries = extract_terms_with_values(dict);
        crate::serialization::bincode_compat::serialize_into(&mut writer, &entries)?;
        Ok(())
    }

    /// Deserialize a [`MappedDictionary`] preserving each term's value.
    pub fn deserialize_with_values<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTermsWithValues,
        D::Value: serde::de::DeserializeOwned,
        R: Read,
    {
        let entries: Vec<(String, D::Value)> =
            crate::serialization::bincode_compat::deserialize_from(&mut reader)?;
        Ok(D::from_terms_with_values(entries))
    }

    /// `serialize_with_values` for `Unit = char` (Unicode) backends.
    ///
    /// Same wire format as [`Self::serialize_with_values`] — a
    /// `Vec<(String, V)>`. The deserialization counterpart is
    /// [`Self::deserialize_with_values`], shared with the byte path.
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
        crate::serialization::bincode_compat::serialize_into(&mut writer, &entries)?;
        Ok(())
    }
}

impl DictionarySerializer for BincodeSerializer {
    fn serialize<D, W>(dict: &D, mut writer: W) -> Result<(), SerializationError>
    where
        D: Dictionary,
        D::Node: DictionaryNode<Unit = u8>,
        W: Write,
    {
        let terms = extract_terms(dict);
        crate::serialization::bincode_compat::serialize_into(&mut writer, &terms)?;
        Ok(())
    }

    fn deserialize<D, R>(mut reader: R) -> Result<D, SerializationError>
    where
        D: DictionaryFromTerms,
        R: Read,
    {
        let terms: Vec<String> =
            crate::serialization::bincode_compat::deserialize_from(&mut reader)?;
        Ok(D::from_terms(terms))
    }
}
