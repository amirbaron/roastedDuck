//! Byte-oriented pattern matching shared by local and remote filesystems.

/// DuckDB's `Glob` (with `?` matching any character).
pub fn glob(string: &[u8], pattern: &[u8]) -> bool {
    let (mut sidx, mut pidx) = (0, 0);
    while sidx < string.len() && pidx < pattern.len() {
        let s = string[sidx];
        let p = pattern[pidx];
        match p {
            b'*' => {
                pidx += 1;
                while pidx < pattern.len() && pattern[pidx] == b'*' {
                    pidx += 1;
                }
                if pidx == pattern.len() {
                    return true;
                }
                return (sidx..string.len()).any(|start| glob(&string[start..], &pattern[pidx..]));
            }
            b'?' => {}
            b'[' => {
                pidx += 1;
                if pidx == pattern.len() {
                    return false;
                }
                let invert = pattern[pidx] == b'!';
                if invert {
                    pidx += 1;
                }
                let mut found_match = invert;
                let start_pos = pidx;
                let mut found_closing_bracket = false;
                while pidx < pattern.len() {
                    let p = pattern[pidx];
                    if p == b']' && pidx > start_pos {
                        found_closing_bracket = true;
                        pidx += 1;
                        break;
                    }
                    if pidx + 1 == pattern.len() {
                        break;
                    }
                    let matches = if pattern[pidx + 1] == b'-' {
                        if pidx + 2 == pattern.len() {
                            break;
                        }
                        let next_char = pattern[pidx + 2];
                        pidx += 3;
                        s >= p && s <= next_char
                    } else {
                        pidx += 1;
                        p == s
                    };
                    if found_match == invert && matches {
                        found_match = !invert;
                    }
                }
                if !found_closing_bracket || !found_match {
                    return false;
                }
                sidx += 1;
                continue;
            }
            b'\\' => {
                pidx += 1;
                if pidx == pattern.len() || s != pattern[pidx] {
                    return false;
                }
            }
            _ => {
                if s != p {
                    return false;
                }
            }
        }
        sidx += 1;
        pidx += 1;
    }
    while pidx < pattern.len() && pattern[pidx] == b'*' {
        pidx += 1;
    }
    pidx == pattern.len() && sidx == string.len()
}
