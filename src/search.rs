// Copyright (C) 2026 George Sapkin
//
// SPDX-License-Identifier: GPL-3.0-only

use fst::automaton::Str;
use fst::{Automaton, IntoStreamer, Map, MapBuilder, Streamer};
use std::collections::{BTreeMap, BTreeSet};

/// A compressed, prefix-searchable index for profile metadata.
///
/// This index uses a Finite State Transducer (FST) to map tokens (words) to
/// sets of profile indices. Stores indices in a contiguous flat vector and uses
/// the FST to store pointers (offsets and lengths) into that vector.
///
/// Once built via [`SearchIndexBuilder`], this structure is immutable and
/// optimized for read-heavy prefix queries.
#[derive(Clone, Default)]
pub struct SearchIndex {
    /// The immutable, compressed FST map.
    map: Option<Map<Vec<u8>>>,
    /// Flat storage for profile indices, indexed by the values in the FST map.
    values: Vec<u32>,
}

impl SearchIndex {
    /// Searches the index for all profile indices associated with the given
    /// `prefix`. Uses the FST map to find keys matching the prefix.
    ///
    /// For every token in the index that starts with `prefix`, it retrieves the
    /// associated profile indices and collects them into a deduplicated
    /// [`BTreeSet`].
    pub fn search(&self, prefix: &str) -> BTreeSet<u32> {
        let mut results = BTreeSet::new();
        let Some(ref map) = self.map else {
            return results;
        };

        let matcher = Str::new(prefix).starts_with();
        let mut stream = map.search(matcher).into_stream();

        while let Some((_, val)) = stream.next() {
            let (offset, len) = Self::unpack_val(val);
            results.extend(&self.values[offset..offset + len]);
        }

        results
    }

    /// Packs a 64-bit FST value containing a 32-bit offset and a 32-bit length.
    /// This allows the FST to map a single string key to a range of values in
    /// the contiguous `values` vector.
    fn pack_val(offset: u64, len: u64) -> u64 {
        (offset << 32) | (len & 0xFFFF_FFFF)
    }

    /// Unpacks a 64-bit FST value into its constituent offset and length.
    #[allow(clippy::cast_possible_truncation)]
    fn unpack_val(val: u64) -> (usize, usize) {
        ((val >> 32) as usize, (val as u32) as usize)
    }
}

/// A builder for constructing a [`SearchIndex`].
///
/// This builder uses a [`BTreeMap`] to collect tokens and their associated
/// profile indices in memory. Using a map during construction ensures that
/// tokens are unique and indices are deduplicated and sorted before they are
/// finalized into the immutable FST.
#[derive(Default)]
pub struct SearchIndexBuilder {
    /// Temporary storage used during construction.
    data: BTreeMap<String, BTreeSet<u32>>,
}

impl SearchIndexBuilder {
    /// Inserts a `word` into the index, associating it with a given profile
    /// `index`.
    ///
    /// Multiple profiles can be associated with the same word. The indices for
    /// a single word are stored in a [`BTreeSet`] to ensure they are unique.
    pub fn insert(&mut self, word: &str, index: u32) {
        self.data.entry(word.to_string()).or_default().insert(index);
    }

    /// Finalizes the index, compressing the temporary builder data into the FST
    /// map.
    ///
    /// This process flattens the profile indices into a single contiguous
    /// vector and constructs the Finite State Transducer. The temporary `data`
    /// is consumed and dropped.
    pub fn build(self) -> SearchIndex {
        if self.data.is_empty() {
            return SearchIndex::default();
        }

        let mut builder = MapBuilder::new(Vec::new()).expect("Failed to create FST builder");
        let mut values = Vec::new();

        for (word, indices) in self.data {
            let offset = values.len() as u64;
            let len = indices.len() as u64;
            values.extend(indices);

            let val = SearchIndex::pack_val(offset, len);
            builder.insert(word, val).expect("FST insertion failed");
        }

        let data = builder.into_inner().expect("FST finalization failed");
        let map = Some(Map::new(data).expect("FST map creation failed"));
        values.shrink_to_fit();

        SearchIndex { map, values }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_index_basic() {
        let mut builder = SearchIndexBuilder::default();
        builder.insert("apple", 1);
        builder.insert("apply", 2);
        builder.insert("banana", 3);
        builder.insert("applet", 4);

        let index = builder.build();

        // Exact match
        assert_eq!(index.search("apple"), BTreeSet::from([1, 4]));

        // Prefix match
        assert_eq!(index.search("app"), BTreeSet::from([1, 2, 4]));
        assert_eq!(index.search("ban"), BTreeSet::from([3]));

        // No match
        assert!(index.search("cherry").is_empty());
    }

    #[test]
    fn test_empty_index() {
        let builder = SearchIndexBuilder::default();
        let index = builder.build();
        assert!(index.search("anything").is_empty());
    }

    #[test]
    fn test_search_empty_prefix() {
        let mut builder = SearchIndexBuilder::default();
        builder.insert("a", 1);
        builder.insert("b", 2);
        let index = builder.build();

        // Empty prefix should return everything
        assert_eq!(index.search(""), BTreeSet::from([1, 2]));
    }
}
