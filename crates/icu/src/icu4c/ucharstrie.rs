//! Port of the matching part of ICU's `UCharsTrie` (`ucharstrie.h`, `ucharstrie.cpp`):
//! the prefix and contraction tries of the collation data.

/// `UStringTrieResult`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UStringTrieResult {
    /// `USTRINGTRIE_NO_MATCH`.
    NoMatch = 0,
    /// `USTRINGTRIE_NO_VALUE`.
    NoValue = 1,
    /// `USTRINGTRIE_FINAL_VALUE`.
    FinalValue = 2,
    /// `USTRINGTRIE_INTERMEDIATE_VALUE`.
    IntermediateValue = 3,
}

impl UStringTrieResult {
    /// `USTRINGTRIE_HAS_VALUE`.
    #[inline]
    pub fn has_value(self) -> bool {
        self as u8 >= Self::FinalValue as u8
    }

    /// `USTRINGTRIE_HAS_NEXT`.
    #[inline]
    pub fn has_next(self) -> bool {
        (self as u8 & 1) != 0
    }
}

const MAX_BRANCH_LINEAR_SUB_NODE_LENGTH: i32 = 5;
const MIN_LINEAR_MATCH: i32 = 0x30;
const MAX_LINEAR_MATCH_LENGTH: i32 = 0x10;
const MIN_VALUE_LEAD: i32 = MIN_LINEAR_MATCH + MAX_LINEAR_MATCH_LENGTH;
const NODE_TYPE_MASK: i32 = MIN_VALUE_LEAD - 1;
const VALUE_IS_FINAL: i32 = 0x8000;
const MAX_ONE_UNIT_VALUE: i32 = 0x3fff;
const MIN_TWO_UNIT_VALUE_LEAD: i32 = MAX_ONE_UNIT_VALUE + 1;
const THREE_UNIT_VALUE_LEAD: i32 = 0x7fff;
const MAX_ONE_UNIT_NODE_VALUE: i32 = 0xff;
const MIN_TWO_UNIT_NODE_VALUE_LEAD: i32 = MIN_VALUE_LEAD + ((MAX_ONE_UNIT_NODE_VALUE + 1) << 6);
const THREE_UNIT_NODE_VALUE_LEAD: i32 = 0x7fc0;
const MAX_ONE_UNIT_DELTA: i32 = 0xfbff;
const MIN_TWO_UNIT_DELTA_LEAD: i32 = MAX_ONE_UNIT_DELTA + 1;
const THREE_UNIT_DELTA_LEAD: i32 = 0xffff;

/// `UCharsTrie::State`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UCharsTrieState {
    /// The address of the trie's units (`uchars`), 0 for no state.
    uchars: usize,
    pos: Option<usize>,
    remaining_match_length: i32,
}

/// A `UCharsTrie` over the units of its serialization.
#[derive(Debug, Clone)]
pub struct UCharsTrie<'a> {
    uchars: &'a [u16],
    /// `pos_`, `None` once stopped.
    pos: Option<usize>,
    /// Remaining length of a linear-match node, minus 1. Negative if not in such a node.
    remaining_match_length: i32,
}

impl<'a> UCharsTrie<'a> {
    /// `UCharsTrie(trieUChars)`.
    pub fn new(uchars: &'a [u16]) -> Self {
        Self {
            uchars,
            pos: Some(0),
            remaining_match_length: -1,
        }
    }

    #[inline]
    fn unit(&self, pos: usize) -> i32 {
        i32::from(self.uchars.get(pos).copied().unwrap_or(0))
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        self.pos = Some(0);
        self.remaining_match_length = -1;
    }

    /// `saveState`.
    pub fn save_state(&self) -> UCharsTrieState {
        UCharsTrieState {
            uchars: self.uchars.as_ptr() as usize,
            pos: self.pos,
            remaining_match_length: self.remaining_match_length,
        }
    }

    /// `resetToState`: only to a state of this trie.
    pub fn reset_to_state(&mut self, state: UCharsTrieState) {
        if state.uchars == self.uchars.as_ptr() as usize && state.uchars != 0 {
            self.pos = state.pos;
            self.remaining_match_length = state.remaining_match_length;
        }
    }

    #[inline]
    fn value_result(node: i32) -> UStringTrieResult {
        if (node >> 15) & 1 != 0 {
            UStringTrieResult::FinalValue
        } else {
            UStringTrieResult::IntermediateValue
        }
    }

    /// `first(uchar)`.
    pub fn first(&mut self, uchar: i32) -> UStringTrieResult {
        self.remaining_match_length = -1;
        self.next_impl(0, uchar)
    }

    /// `firstForCodePoint(cp)`.
    pub fn first_for_code_point(&mut self, cp: i32) -> UStringTrieResult {
        if cp <= 0xffff {
            self.first(cp)
        } else if self.first(lead(cp)).has_next() {
            self.next(trail(cp))
        } else {
            UStringTrieResult::NoMatch
        }
    }

    /// `nextForCodePoint(cp)`.
    pub fn next_for_code_point(&mut self, cp: i32) -> UStringTrieResult {
        if cp <= 0xffff {
            self.next(cp)
        } else if self.next(lead(cp)).has_next() {
            self.next(trail(cp))
        } else {
            UStringTrieResult::NoMatch
        }
    }

    /// `next(uchar)`.
    pub fn next(&mut self, uchar: i32) -> UStringTrieResult {
        let Some(mut pos) = self.pos else {
            return UStringTrieResult::NoMatch;
        };
        let mut length = self.remaining_match_length;
        if length >= 0 {
            // Remaining part of a linear-match node.
            let unit = self.unit(pos);
            pos += 1;
            if uchar == unit {
                length -= 1;
                self.remaining_match_length = length;
                self.pos = Some(pos);
                let node = self.unit(pos);
                return if length < 0 && node >= MIN_VALUE_LEAD {
                    Self::value_result(node)
                } else {
                    UStringTrieResult::NoValue
                };
            }
            self.pos = None;
            return UStringTrieResult::NoMatch;
        }
        self.next_impl(pos, uchar)
    }

    fn next_impl(&mut self, mut pos: usize, uchar: i32) -> UStringTrieResult {
        let mut node = self.unit(pos);
        pos += 1;
        loop {
            if node < MIN_LINEAR_MATCH {
                return self.branch_next(pos, node, uchar);
            } else if node < MIN_VALUE_LEAD {
                // Match the first of length+1 units.
                let length = node - MIN_LINEAR_MATCH; // Actual match length minus 1.
                let unit = self.unit(pos);
                pos += 1;
                if uchar == unit {
                    let length = length - 1;
                    self.remaining_match_length = length;
                    self.pos = Some(pos);
                    node = self.unit(pos);
                    return if length < 0 && node >= MIN_VALUE_LEAD {
                        Self::value_result(node)
                    } else {
                        UStringTrieResult::NoValue
                    };
                }
                // No match.
                break;
            } else if node & VALUE_IS_FINAL != 0 {
                // No further matching units.
                break;
            }
            // Skip intermediate value.
            pos = Self::skip_node_value(pos, node);
            node &= NODE_TYPE_MASK;
        }
        self.pos = None;
        UStringTrieResult::NoMatch
    }

    fn branch_next(&mut self, mut pos: usize, mut length: i32, uchar: i32) -> UStringTrieResult {
        // Branch according to the current unit.
        if length == 0 {
            length = self.unit(pos);
            pos += 1;
        }
        length += 1;
        // The length of the branch is the number of units to select from.
        // The data structure encodes a binary search.
        while length > MAX_BRANCH_LINEAR_SUB_NODE_LENGTH {
            let unit = self.unit(pos);
            pos += 1;
            if uchar < unit {
                length >>= 1;
                pos = self.jump_by_delta(pos);
            } else {
                length -= length >> 1;
                pos = self.skip_delta(pos);
            }
        }
        // Drop down to linear search for the last few units.
        loop {
            let unit = self.unit(pos);
            pos += 1;
            if uchar == unit {
                let mut node = self.unit(pos);
                let result = if node & VALUE_IS_FINAL != 0 {
                    // Leave the final value for getValue() to read.
                    UStringTrieResult::FinalValue
                } else {
                    // Use the non-final value as the jump delta.
                    pos += 1;
                    let delta = if node < MIN_TWO_UNIT_VALUE_LEAD {
                        node
                    } else if node < THREE_UNIT_VALUE_LEAD {
                        let delta = ((node - MIN_TWO_UNIT_VALUE_LEAD) << 16) | self.unit(pos);
                        pos += 1;
                        delta
                    } else {
                        let delta = (self.unit(pos) << 16) | self.unit(pos + 1);
                        pos += 2;
                        delta
                    };
                    pos = pos.wrapping_add_signed(delta as isize);
                    node = self.unit(pos);
                    if node >= MIN_VALUE_LEAD {
                        Self::value_result(node)
                    } else {
                        UStringTrieResult::NoValue
                    }
                };
                self.pos = Some(pos);
                return result;
            }
            length -= 1;
            pos = self.skip_value_at(pos);
            if length <= 1 {
                break;
            }
        }
        let unit = self.unit(pos);
        pos += 1;
        if uchar == unit {
            self.pos = Some(pos);
            let node = self.unit(pos);
            if node >= MIN_VALUE_LEAD {
                Self::value_result(node)
            } else {
                UStringTrieResult::NoValue
            }
        } else {
            self.pos = None;
            UStringTrieResult::NoMatch
        }
    }

    /// `getValue()`: the value of the last match; requires a value result.
    pub fn get_value(&self) -> i32 {
        let Some(mut pos) = self.pos else {
            return 0;
        };
        let lead_unit = self.unit(pos);
        pos += 1;
        if lead_unit & VALUE_IS_FINAL != 0 {
            self.read_value(pos, lead_unit & 0x7fff)
        } else {
            self.read_node_value(pos, lead_unit)
        }
    }

    fn read_value(&self, pos: usize, lead_unit: i32) -> i32 {
        if lead_unit < MIN_TWO_UNIT_VALUE_LEAD {
            lead_unit
        } else if lead_unit < THREE_UNIT_VALUE_LEAD {
            ((lead_unit - MIN_TWO_UNIT_VALUE_LEAD) << 16) | self.unit(pos)
        } else {
            (self.unit(pos) << 16) | self.unit(pos + 1)
        }
    }

    fn read_node_value(&self, pos: usize, lead_unit: i32) -> i32 {
        if lead_unit < MIN_TWO_UNIT_NODE_VALUE_LEAD {
            (lead_unit >> 6) - 1
        } else if lead_unit < THREE_UNIT_NODE_VALUE_LEAD {
            (((lead_unit & 0x7fc0) - MIN_TWO_UNIT_NODE_VALUE_LEAD) << 10) | self.unit(pos)
        } else {
            (self.unit(pos) << 16) | self.unit(pos + 1)
        }
    }

    /// `skipValue(pos)`: skips the value whose lead unit is at `pos`.
    fn skip_value_at(&self, pos: usize) -> usize {
        let lead_unit = self.unit(pos) & 0x7fff;
        Self::skip_value(pos + 1, lead_unit)
    }

    fn skip_value(pos: usize, lead_unit: i32) -> usize {
        if lead_unit >= MIN_TWO_UNIT_VALUE_LEAD {
            if lead_unit < THREE_UNIT_VALUE_LEAD {
                pos + 1
            } else {
                pos + 2
            }
        } else {
            pos
        }
    }

    fn skip_node_value(pos: usize, lead_unit: i32) -> usize {
        if lead_unit >= MIN_TWO_UNIT_NODE_VALUE_LEAD {
            if lead_unit < THREE_UNIT_NODE_VALUE_LEAD {
                pos + 1
            } else {
                pos + 2
            }
        } else {
            pos
        }
    }

    fn jump_by_delta(&self, mut pos: usize) -> usize {
        let mut delta = self.unit(pos);
        pos += 1;
        if delta >= MIN_TWO_UNIT_DELTA_LEAD {
            if delta == THREE_UNIT_DELTA_LEAD {
                delta = (self.unit(pos) << 16) | self.unit(pos + 1);
                pos += 2;
            } else {
                delta = ((delta - MIN_TWO_UNIT_DELTA_LEAD) << 16) | self.unit(pos);
                pos += 1;
            }
        }
        pos.wrapping_add_signed(delta as isize)
    }

    fn skip_delta(&self, mut pos: usize) -> usize {
        let delta = self.unit(pos);
        pos += 1;
        if delta >= MIN_TWO_UNIT_DELTA_LEAD {
            if delta == THREE_UNIT_DELTA_LEAD {
                pos += 2;
            } else {
                pos += 1;
            }
        }
        pos
    }
}

/// `U16_LEAD(cp)`.
#[inline]
pub fn lead(cp: i32) -> i32 {
    (cp >> 10) + 0xd7c0
}

/// `U16_TRAIL(cp)`.
#[inline]
pub fn trail(cp: i32) -> i32 {
    (cp & 0x3ff) | 0xdc00
}
