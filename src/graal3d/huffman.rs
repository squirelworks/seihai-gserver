//! Torque's string coding: a fixed Huffman tree over byte values.
//!
//! Every Graal3D build codes strings with the tree that Torque builds
//! from one fixed table of character counts. The table and the tree
//! rules below follow the open Torque 3D source
//! (`core/stream/bitStream.cpp`):
//!
//! > Copyright (c) 2012 GarageGames, LLC. Permission is hereby granted,
//! > free of charge, to any person obtaining a copy of this software and
//! > associated documentation files (the "Software"), to deal in the
//! > Software without restriction, including without limitation the
//! > rights to use, copy, modify, merge, publish, distribute, sublicense,
//! > and/or sell copies of the Software, and to permit persons to whom the
//! > Software is furnished to do so, subject to the following conditions:
//! > The above copyright notice and this permission notice shall be
//! > included in all copies or substantial portions of the Software.
//! > THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.
//!
//! The tree has to come out bit for bit the same as the client's, so the
//! merge order (the two lowest counts, ties to the earlier slot, the
//! merged node kept in the lower slot and the last slot moved into the
//! freed one) is kept exactly.

use std::sync::OnceLock;

/// How often each byte value appears in Torque's sample text. Each
/// leaf's weight is its count + 1.
const CHAR_FREQS: [u32; 256] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 329, 21, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 2809, 68, 0, 27, 0, 58, 3, 62, 4, 7, 0, 0, 15, 65, 554, 3, 394, 404, 189, 117, 30, 51, 27,
    15, 34, 32, 80, 1, 142, 3, 142, 39, 0, 144, 125, 44, 122, 275, 70, 135, 61, 127, 8, 12, 113,
    246, 122, 36, 185, 1, 149, 309, 335, 12, 11, 14, 54, 151, 0, 0, 2, 0, 0, 211, 0, 2090, 344,
    736, 993, 2872, 701, 605, 646, 1552, 328, 305, 1240, 735, 1533, 1713, 562, 3, 1775, 1149, 1469,
    979, 407, 553, 59, 279, 31, 0, 0, 0, 68, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

/// A child of a tree node: another node, or a leaf with a byte value.
#[derive(Debug, Clone, Copy)]
enum Child {
    Node(usize),
    Leaf(u8),
}

/// The Huffman tree and each byte's code.
pub struct Huffman {
    /// Node 0 is the root. Each node has its 0 and its 1 child.
    nodes: Vec<[Child; 2]>,
    /// Per byte: the code bits in the order they are written, bit 0 first,
    /// and how many there are.
    codes: [(u32, u8); 256],
}

impl Huffman {
    fn build() -> Self {
        #[derive(Clone, Copy)]
        struct Wrap {
            weight: u32,
            child: Child,
        }
        // Node 0 is a placeholder; the root is copied into it at the end.
        let mut nodes: Vec<[Child; 2]> = vec![[Child::Leaf(0); 2]];
        let mut wraps: Vec<Wrap> = (0..256)
            .map(|i| Wrap {
                weight: CHAR_FREQS[i] + 1,
                child: Child::Leaf(i as u8),
            })
            .collect();
        while wraps.len() > 1 {
            let (mut min1, mut min2) = (0xffff_fffe_u32, 0xffff_ffff_u32);
            let (mut i1, mut i2) = (usize::MAX, usize::MAX);
            for (i, w) in wraps.iter().enumerate() {
                if w.weight < min1 {
                    min2 = min1;
                    i2 = i1;
                    min1 = w.weight;
                    i1 = i;
                } else if w.weight < min2 {
                    min2 = w.weight;
                    i2 = i;
                }
            }
            nodes.push([wraps[i1].child, wraps[i2].child]);
            let merged = Wrap {
                weight: wraps[i1].weight + wraps[i2].weight,
                child: Child::Node(nodes.len() - 1),
            };
            let (keep, drop) = (i1.min(i2), i1.max(i2));
            wraps[keep] = merged;
            let last = wraps.len() - 1;
            wraps[drop] = wraps[last];
            wraps.pop();
        }
        let Child::Node(root) = wraps[0].child else {
            unreachable!("256 leaves make a node")
        };
        nodes[0] = nodes[root];

        let mut codes = [(0u32, 0u8); 256];
        // Walk the tree; the path to a leaf is its code, first step in bit 0.
        let mut stack = vec![(Child::Node(0), 0u32, 0u8)];
        while let Some((child, code, depth)) = stack.pop() {
            match child {
                Child::Leaf(b) => codes[b as usize] = (code, depth),
                Child::Node(n) => {
                    let [zero, one] = nodes[n];
                    stack.push((zero, code, depth + 1));
                    stack.push((one, code | 1 << depth, depth + 1));
                }
            }
        }
        Huffman { nodes, codes }
    }

    /// The one tree, built on first use.
    pub fn get() -> &'static Huffman {
        static TREE: OnceLock<Huffman> = OnceLock::new();
        TREE.get_or_init(Huffman::build)
    }

    /// The code of `byte`: its bits in write order (bit 0 first) and its length.
    pub fn code(&self, byte: u8) -> (u32, u8) {
        self.codes[byte as usize]
    }

    /// Follows the tree from the root, one bit per step, to a byte.
    /// `next_bit` gives `None` when the input runs out.
    pub fn decode(&self, mut next_bit: impl FnMut() -> Option<bool>) -> Option<u8> {
        let mut node = 0;
        loop {
            match self.nodes[node][next_bit()? as usize] {
                Child::Leaf(b) => return Some(b),
                Child::Node(n) => node = n,
            }
        }
    }

    /// How many bits `bytes` take when coded.
    pub fn coded_bits(&self, bytes: &[u8]) -> usize {
        bytes
            .iter()
            .map(|&b| self.codes[b as usize].1 as usize)
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_a_prefix_free_set_over_every_byte() {
        let h = Huffman::get();
        // Kraft equality: a full binary tree over 256 leaves.
        let kraft: f64 = (0..=255u8).map(|b| 0.5f64.powi(h.code(b).1 as i32)).sum();
        assert!((kraft - 1.0).abs() < 1e-12, "{kraft}");
        for b in 0..=255u8 {
            let (code, len) = h.code(b);
            assert!(len > 0 && len <= 32, "byte {b}: {len} bits");
            let mut i = 0;
            assert_eq!(
                h.decode(|| {
                    let bit = code >> i & 1 == 1;
                    i += 1;
                    Some(bit)
                }),
                Some(b)
            );
            assert_eq!(i, len as u32);
        }
    }

    #[test]
    fn codes_match_torques_own_tree_build() {
        // Torque's buildTables/generateCodes compiled from the MIT source
        // and run on the same table gave these (code bits in write order).
        let h = Huffman::get();
        assert_eq!(h.code(b' '), (7, 4));
        assert_eq!(h.code(b'e'), (15, 4));
        assert_eq!(h.code(b'G'), (177, 8));
        assert_eq!(h.code(b'\t'), (75, 7));
        assert_eq!(h.code(0), (4622, 15));
    }

    #[test]
    fn common_text_is_short() {
        let h = Huffman::get();
        // Space and 'e' are the commonest bytes in the table.
        assert!(h.code(b' ').1 <= 4);
        assert!(h.code(b'e').1 <= 4);
        assert!(h.coded_bits(b"GameConnection") < 14 * 8);
        // A byte that never appears gets a long code.
        assert!(h.code(0xff).1 > 8);
    }
}
