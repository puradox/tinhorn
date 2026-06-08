//! Dice-notation parser.
//!
//! Understands the common forms and a few conveniences:
//!   "3d6"        -> three six-sided dice
//!   "d6+d8"      -> one d6 and one d8, summed
//!   "d6d10"      -> adjacency works as a separator too
//!   "d6,d12"     -> commas work as separators
//!   "2d20-1"     -> dice plus a flat modifier
//!   "d20 + 5"    -> whitespace is ignored
//!
//! Grammar (loosely):
//!   expr  := term ( sep? term )*
//!   sep   := '+' | '-' | ',' | whitespace
//!   term  := [count] ('d'|'D') sides   |   integer
//!
//! A leading '-' applies to the following flat modifier (dice are always added).

/// One physical die to be thrown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DieSpec {
    pub sides: u32,
}

/// The fully-parsed request: a pool of individual dice plus a flat modifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roll {
    pub dice: Vec<DieSpec>,
    pub modifier: i32,
}

// Sanity limits so "999d999999" can't wedge the renderer or the RNG loop.
const MAX_DICE: usize = 60;
const MAX_SIDES: u32 = 1000;

pub fn parse(input: &str) -> Result<Roll, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let n = chars.len();

    let mut dice: Vec<DieSpec> = Vec::new();
    let mut modifier: i32 = 0;
    let mut sign: i32 = 1;
    let mut saw_term = false;

    while i < n {
        let c = chars[i];

        // Separators / sign markers.
        if c.is_whitespace() || c == ',' {
            i += 1;
            continue;
        }
        if c == '+' {
            sign = 1;
            i += 1;
            continue;
        }
        if c == '-' {
            sign = -1;
            i += 1;
            continue;
        }

        // A term begins: optional leading count, then maybe 'd<sides>'.
        let num_start = i;
        while i < n && chars[i].is_ascii_digit() {
            i += 1;
        }
        let num_str: String = chars[num_start..i].iter().collect();

        if i < n && (chars[i] == 'd' || chars[i] == 'D') {
            // Dice term: [count]d<sides>
            i += 1; // consume 'd'
            let sides_start = i;
            while i < n && chars[i].is_ascii_digit() {
                i += 1;
            }
            let sides_str: String = chars[sides_start..i].iter().collect();
            if sides_str.is_empty() {
                return Err(format!("expected a number of sides after 'd' (e.g. d6)"));
            }

            let count: u32 = if num_str.is_empty() {
                1
            } else {
                num_str
                    .parse()
                    .map_err(|_| format!("'{num_str}' is not a valid dice count"))?
            };
            let sides: u32 = sides_str
                .parse()
                .map_err(|_| format!("'{sides_str}' is not a valid number of sides"))?;

            if sides == 0 {
                return Err("a die needs at least 1 side".to_string());
            }
            if sides > MAX_SIDES {
                return Err(format!("that's a lot of sides — keep it under {MAX_SIDES}"));
            }
            if dice.len() + count as usize > MAX_DICE {
                return Err(format!("too many dice — keep the pool under {MAX_DICE}"));
            }

            for _ in 0..count {
                dice.push(DieSpec { sides });
            }
            saw_term = true;
            sign = 1; // sign only ever applied to flat modifiers
        } else if !num_str.is_empty() {
            // Flat modifier.
            let v: i32 = num_str
                .parse()
                .map_err(|_| format!("'{num_str}' is not a valid number"))?;
            modifier += sign * v;
            saw_term = true;
            sign = 1;
        } else {
            return Err(format!("unexpected character '{c}'"));
        }
    }

    if !saw_term {
        return Err("type some dice, e.g. 3d6 or d20+d4".to_string());
    }
    if dice.is_empty() {
        return Err("no dice — a roll needs at least one 'd' term".to_string());
    }

    Ok(Roll { dice, modifier })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sides(r: &Roll) -> Vec<u32> {
        r.dice.iter().map(|d| d.sides).collect()
    }

    #[test]
    fn count_and_sides() {
        let r = parse("3d6").unwrap();
        assert_eq!(sides(&r), vec![6, 6, 6]);
        assert_eq!(r.modifier, 0);
    }

    #[test]
    fn plus_separator() {
        let r = parse("d6+d8").unwrap();
        assert_eq!(sides(&r), vec![6, 8]);
    }

    #[test]
    fn adjacency_separator() {
        let r = parse("d6d10").unwrap();
        assert_eq!(sides(&r), vec![6, 10]);
    }

    #[test]
    fn comma_separator() {
        let r = parse("d6,d12").unwrap();
        assert_eq!(sides(&r), vec![6, 12]);
    }

    #[test]
    fn modifier_add_and_subtract() {
        let r = parse("2d20-1").unwrap();
        assert_eq!(sides(&r), vec![20, 20]);
        assert_eq!(r.modifier, -1);

        let r = parse("d8 + 5").unwrap();
        assert_eq!(r.modifier, 5);
    }

    #[test]
    fn case_insensitive_d() {
        let r = parse("2D6").unwrap();
        assert_eq!(sides(&r), vec![6, 6]);
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("d").is_err());
        assert!(parse("5").is_err()); // modifier with no dice
        assert!(parse("d0").is_err());
        assert!(parse("xyz").is_err());
    }
}
