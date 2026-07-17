//! Dice-notation parser.
//!
//! Understands the common forms and a few conveniences:
//!   "3d6"        -> three six-sided dice
//!   "d6+d8"      -> one d6 and one d8, summed
//!   "d6d10"      -> adjacency works as a separator too
//!   "d6,d12"     -> commas work as separators
//!   "2d20-1"     -> dice plus a flat modifier
//!   "d20 + 5"    -> whitespace is ignored
//!   "d%"         -> percentile: shorthand for d100
//!
//! Per-term modifiers (written immediately after the `dN`, in any order):
//!   "2d20kh1"    -> keep the highest 1 (advantage); "kl1" keeps the lowest
//!   "4d6dl1"     -> drop the lowest 1 (classic ability score); "dh1" drops highest
//!   "3d6!"       -> exploding: a max face rolls another die (repeats, capped)
//!   "d10!>8"     -> exploding on >8 instead of just the max face
//!   "4d6*2"      -> multiply this term's kept sum by 2
//!
//! Stakes (at most one, anywhere a separator could go — conventionally last):
//!   "d20+5 > 15"  -> meet or beat a target: total >= 15 succeeds
//!   "d20+5 vs 15" -> the same thing; 'vs' is an alias for '>'
//!   "d20 < 10"    -> roll-under: total <= 10 succeeds
//! Both comparisons are inclusive — you win *on* the number.
//!
//! Grammar (loosely):
//!   expr  := term ( sep? term )* [ ('vs'|'VS'|'>'|'<') n ]
//!   sep   := '+' | '-' | ',' | whitespace
//!   term  := [count] ('d'|'D') sides termmod*   |   integer
//!   sides := digits | '%'                       ('%' means 100)
//!   termmod := ('kh'|'kl'|'dh'|'dl') [n]
//!            |  '!' [ ('>'|'<'|'=') n ]
//!            |  '*' n
//!
//! A leading '-' applies to the following flat modifier (dice are always added).

/// A comparison used by exploding ("blow up when the face meets this").
/// A bare `!` desugars to `Eq(sides)` once the term's die size is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compare {
    Eq(u32),
    Lt(u32),
    Gt(u32),
}

impl Compare {
    pub fn matches(self, v: u32) -> bool {
        match self {
            Compare::Eq(n) => v == n,
            Compare::Lt(n) => v < n,
            Compare::Gt(n) => v > n,
        }
    }
}

/// One modifier attached to a dice term, applied to that term's pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermMod {
    KeepHigh(u32),
    KeepLow(u32),
    DropHigh(u32),
    DropLow(u32),
    /// `None` means "explode on the max face" — resolved against `sides` at roll time.
    Explode(Option<Compare>),
    /// Multiply this term's kept sum by the constant.
    Mul(i32),
}

/// One dice term before rolling: a homogeneous pool plus its modifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiceTerm {
    pub count: u32,
    pub sides: u32,
    pub mods: Vec<TermMod>,
}

/// Which way a staked roll is won: reach the target from below or from above.
/// `> N` (and its word alias `vs N`) is meet-or-beat; `< N` flips it to
/// roll-under. Serialized in `--json` (`"goal": "over"` / `"under"`) so
/// consumers read the direction rather than re-derive it. See [`super::app::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Goal {
    /// Meet-or-beat (`>`): the total must reach the target (`total >= target`).
    Over,
    /// Roll-under (`<`): the total must come in at or below it (`total <= target`).
    Under,
}

/// The stakes on a roll: a target plus the direction the total must reach it
/// from. Bundling the two makes "a direction only means something when there
/// is a target" unrepresentable — an unstaked roll is simply `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stake {
    pub target: i32,
    pub goal: Goal,
}

impl Stake {
    /// The compact chip text, e.g. `vs 15` or `vs ≤10`. Shared by the arena
    /// verdict chip, the stats pane, and the CLI breakdown so the three can
    /// never disagree about how a stake is spelled, and so the roll-under `≤`
    /// is always shown next to its target.
    pub fn label(&self) -> String {
        match self.goal {
            Goal::Over => format!("vs {}", self.target),
            Goal::Under => format!("vs ≤{}", self.target),
        }
    }
}

/// The fully-parsed request: a sequence of dice terms plus a flat modifier,
/// and — when the roll has stakes — the target the total is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Roll {
    pub terms: Vec<DiceTerm>,
    pub modifier: i32,
    /// The stakes (`vs N`), when the roll is staked.
    pub stake: Option<Stake>,
}

// Sanity limits so "999d999999" can't wedge the renderer or the RNG loop.
const MAX_DICE: usize = 60;
const MAX_SIDES: u32 = 1000;

pub fn parse(input: &str) -> Result<Roll, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let n = chars.len();

    let mut terms: Vec<DiceTerm> = Vec::new();
    let mut base_dice = 0usize; // running count of base dice, for the pool cap
    let mut modifier: i32 = 0;
    let mut sign: i32 = 1;
    let mut saw_term = false;
    let mut stake: Option<Stake> = None;

    while i < n {
        let c = chars[i];

        // Separators / sign markers.
        if c.is_whitespace() || c == ',' {
            i += 1;
            continue;
        }

        // Stakes: name a target the total is checked against. `>` (and its
        // word alias `vs`) is meet-or-beat; `<` is roll-under. Matched before
        // term parsing so the introducer can't fall through to "unexpected
        // character". The target must end the roll: allowing dice after it
        // would make a contested-roll attempt like `d20 vs 4d6` silently parse
        // as "target 4, plus a d6 for me" — an error beats a misparse.
        if let Some(goal) = stake_goal(&chars, &mut i) {
            if stake.is_some() {
                return Err("only one target per roll".to_string());
            }
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            let target = parse_required_int(
                &chars,
                &mut i,
                "expected a target after 'vs' / '>' / '<' (e.g. d20 vs 15 or d20 < 10)",
            )?;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            if i < n {
                return Err("the target must come last (e.g. d20+5 vs 15)".to_string());
            }
            stake = Some(Stake { target, goal });
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
            // Dice term: [count]d<sides>[mods...]
            i += 1; // consume 'd'
            let sides: u32 = if i < n && chars[i] == '%' {
                // Percentile: 'd%' is the tabletop spelling of a d100.
                i += 1;
                100
            } else {
                let sides_start = i;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let sides_str: String = chars[sides_start..i].iter().collect();
                if sides_str.is_empty() {
                    return Err("expected a number of sides after 'd' (e.g. d6 or d%)".to_string());
                }
                sides_str
                    .parse()
                    .map_err(|_| format!("'{sides_str}' is not a valid number of sides"))?
            };

            let count: u32 = if num_str.is_empty() {
                1
            } else {
                num_str
                    .parse()
                    .map_err(|_| format!("'{num_str}' is not a valid dice count"))?
            };

            if sides == 0 {
                return Err("a die needs at least 1 side".to_string());
            }
            if sides > MAX_SIDES {
                return Err(format!("that's a lot of sides — keep it under {MAX_SIDES}"));
            }
            base_dice += count as usize;
            if base_dice > MAX_DICE {
                return Err(format!("too many dice — keep the pool under {MAX_DICE}"));
            }

            let mods = parse_term_mods(&chars, &mut i, count, sides)?;

            terms.push(DiceTerm { count, sides, mods });
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
    if terms.is_empty() {
        return Err("no dice — a roll needs at least one 'd' term".to_string());
    }

    Ok(Roll {
        terms,
        modifier,
        stake,
    })
}

/// Match the stakes keyword `vs` at `pos` without consuming (case-insensitive).
fn vs_keyword(chars: &[char], pos: usize) -> bool {
    matches!(chars.get(pos), Some('v' | 'V')) && matches!(chars.get(pos + 1), Some('s' | 'S'))
}

/// Detect a stake introducer at `i` and, on a match, consume it and name the
/// direction: `>` (and its word alias `vs`) is meet-or-beat ([`Goal::Over`]),
/// `<` is roll-under ([`Goal::Under`]). Both are inclusive — a total equal to
/// the target wins either way — so there is no separate `>=`/`<=`. Returns
/// `None` and leaves `i` untouched when no introducer is present, so the
/// character falls through to the rest of the parse.
fn stake_goal(chars: &[char], i: &mut usize) -> Option<Goal> {
    if vs_keyword(chars, *i) {
        *i += 2;
        return Some(Goal::Over);
    }
    let goal = match chars.get(*i) {
        Some('>') => Goal::Over,
        Some('<') => Goal::Under,
        _ => return None,
    };
    *i += 1;
    Some(goal)
}

/// Consume any run of term modifiers (`kh`/`kl`/`dh`/`dl`/`!`/`*`) sitting right
/// after a `dN`. `i` points just past the sides digits on entry and is advanced
/// past every modifier consumed. `count`/`sides` are used to validate.
///
/// The keep/drop forms start with `d` (`dl`, `dh`), which must be matched here
/// *before* the outer loop would treat a bare `d` as an adjacency separator —
/// otherwise `4d6dl1` would read as `4d6` next to `d`-with-no-sides.
fn parse_term_mods(
    chars: &[char],
    i: &mut usize,
    count: u32,
    sides: u32,
) -> Result<Vec<TermMod>, String> {
    let n = chars.len();
    let mut mods: Vec<TermMod> = Vec::new();

    loop {
        // Keep/drop: a two-letter keyword optionally followed by a count (default 1).
        if let Some(kind) = keep_drop_keyword(chars, *i) {
            *i += 2;
            let k = parse_optional_count(chars, i).unwrap_or(1);
            if k == 0 {
                return Err("keep/drop count must be at least 1".to_string());
            }
            // Keeping/dropping more than the pool has is harmless; clamp it.
            let k = k.min(count);
            mods.push(match kind {
                KeepDrop::KeepHigh => TermMod::KeepHigh(k),
                KeepDrop::KeepLow => TermMod::KeepLow(k),
                KeepDrop::DropHigh => TermMod::DropHigh(k),
                KeepDrop::DropLow => TermMod::DropLow(k),
            });
            continue;
        }

        // Exploding: '!' optionally followed by a comparison.
        if *i < n && chars[*i] == '!' {
            *i += 1;
            let cmp = parse_optional_compare(chars, i)?;
            // A bare '!' on a 1-sided die would explode forever; the roll-time
            // cap stops runaway growth, but flag the obviously-degenerate case.
            if sides == 1 && cmp.is_none() {
                return Err("a d1 always rolls its max — it can't explode".to_string());
            }
            mods.push(TermMod::Explode(cmp));
            continue;
        }

        // Multiply this term's sum by a constant.
        if *i < n && chars[*i] == '*' {
            *i += 1;
            let m = parse_required_int(chars, i, "expected a number after '*' (e.g. 4d6*2)")?;
            mods.push(TermMod::Mul(m));
            continue;
        }

        break;
    }

    Ok(mods)
}

enum KeepDrop {
    KeepHigh,
    KeepLow,
    DropHigh,
    DropLow,
}

/// Match a keep/drop keyword at `pos` without consuming. Case-insensitive on the
/// leading letter so `KH`/`kh` both work.
fn keep_drop_keyword(chars: &[char], pos: usize) -> Option<KeepDrop> {
    let a = *chars.get(pos)?;
    let b = *chars.get(pos + 1)?;
    let (a, b) = (a.to_ascii_lowercase(), b.to_ascii_lowercase());
    match (a, b) {
        ('k', 'h') => Some(KeepDrop::KeepHigh),
        ('k', 'l') => Some(KeepDrop::KeepLow),
        ('d', 'h') => Some(KeepDrop::DropHigh),
        ('d', 'l') => Some(KeepDrop::DropLow),
        _ => None,
    }
}

/// Read an optional run of digits at `i`, advancing past them. Returns `None`
/// when no digit follows (so `kh` alone means `kh1`).
fn parse_optional_count(chars: &[char], i: &mut usize) -> Option<u32> {
    let start = *i;
    while *i < chars.len() && chars[*i].is_ascii_digit() {
        *i += 1;
    }
    if *i == start {
        return None;
    }
    chars[start..*i].iter().collect::<String>().parse().ok()
}

/// Parse an optional explode comparison: `>N`, `<N`, `=N`, or nothing.
fn parse_optional_compare(chars: &[char], i: &mut usize) -> Result<Option<Compare>, String> {
    let op = match chars.get(*i) {
        Some('>') => Compare::Gt as fn(u32) -> Compare,
        Some('<') => Compare::Lt as fn(u32) -> Compare,
        Some('=') => Compare::Eq as fn(u32) -> Compare,
        _ => return Ok(None),
    };
    *i += 1;
    let n = parse_optional_count(chars, i)
        .ok_or_else(|| "expected a number after the explode comparison (e.g. !>8)".to_string())?;
    Ok(Some(op(n)))
}

/// Parse a required signed integer at `i` (used by `*` and `vs`). Advances
/// past it. Range-checked: a magnitude that fits u32 but not i32 must error
/// rather than wrap negative (a `vs 3000000000` that silently becomes a huge
/// negative target would succeed on every roll).
fn parse_required_int(chars: &[char], i: &mut usize, msg: &str) -> Result<i32, String> {
    let neg = matches!(chars.get(*i), Some('-'));
    if neg || matches!(chars.get(*i), Some('+')) {
        *i += 1;
    }
    let mag = parse_optional_count(chars, i).ok_or_else(|| msg.to_string())? as i64;
    let v = if neg { -mag } else { mag };
    i32::try_from(v).map_err(|_| "that number is too large".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(r: &Roll, idx: usize) -> &DiceTerm {
        &r.terms[idx]
    }

    #[test]
    fn count_and_sides() {
        let r = parse("3d6").unwrap();
        assert_eq!(term(&r, 0).count, 3);
        assert_eq!(term(&r, 0).sides, 6);
        assert!(term(&r, 0).mods.is_empty());
        assert_eq!(r.modifier, 0);
    }

    #[test]
    fn plus_separator() {
        let r = parse("d6+d8").unwrap();
        assert_eq!(term(&r, 0).sides, 6);
        assert_eq!(term(&r, 1).sides, 8);
    }

    #[test]
    fn adjacency_separator() {
        let r = parse("d6d10").unwrap();
        assert_eq!(term(&r, 0).sides, 6);
        assert_eq!(term(&r, 1).sides, 10);
    }

    #[test]
    fn comma_separator() {
        let r = parse("d6,d12").unwrap();
        assert_eq!(term(&r, 0).sides, 6);
        assert_eq!(term(&r, 1).sides, 12);
    }

    #[test]
    fn modifier_add_and_subtract() {
        let r = parse("2d20-1").unwrap();
        assert_eq!(term(&r, 0).count, 2);
        assert_eq!(r.modifier, -1);

        let r = parse("d8 + 5").unwrap();
        assert_eq!(r.modifier, 5);
    }

    #[test]
    fn case_insensitive_d() {
        let r = parse("2D6").unwrap();
        assert_eq!(term(&r, 0).count, 2);
        assert_eq!(term(&r, 0).sides, 6);
    }

    #[test]
    fn percentile_shorthand() {
        // 'd%' is a d100, count and case behave like any other die.
        let r = parse("d%").unwrap();
        assert_eq!(term(&r, 0).count, 1);
        assert_eq!(term(&r, 0).sides, 100);

        let r = parse("2D%+5").unwrap();
        assert_eq!(term(&r, 0).count, 2);
        assert_eq!(term(&r, 0).sides, 100);
        assert_eq!(r.modifier, 5);

        // Term modifiers and stakes attach exactly as they would to 'd100'.
        let r = parse("2d%kh1 vs 60").unwrap();
        assert_eq!(term(&r, 0).mods, vec![TermMod::KeepHigh(1)]);
        assert_eq!(
            r.stake,
            Some(Stake {
                target: 60,
                goal: Goal::Over
            })
        );

        // '%' anywhere else is still an error.
        assert!(parse("%").is_err());
        assert!(parse("3%").is_err());
    }

    #[test]
    fn keep_high_and_low() {
        let r = parse("2d20kh1").unwrap(); // advantage
        assert_eq!(term(&r, 0).mods, vec![TermMod::KeepHigh(1)]);

        let r = parse("2d20kl1").unwrap(); // disadvantage
        assert_eq!(term(&r, 0).mods, vec![TermMod::KeepLow(1)]);

        // bare kh defaults to 1
        let r = parse("2d20kh").unwrap();
        assert_eq!(term(&r, 0).mods, vec![TermMod::KeepHigh(1)]);
    }

    #[test]
    fn drop_low_is_not_confused_with_adjacency() {
        // The leading 'd' of 'dl' must not be read as an adjacency separator.
        let r = parse("4d6dl1").unwrap();
        assert_eq!(r.terms.len(), 1);
        assert_eq!(term(&r, 0).count, 4);
        assert_eq!(term(&r, 0).mods, vec![TermMod::DropLow(1)]);
    }

    #[test]
    fn keep_drop_clamps_to_pool() {
        let r = parse("4d6kh9").unwrap();
        assert_eq!(term(&r, 0).mods, vec![TermMod::KeepHigh(4)]);
    }

    #[test]
    fn exploding() {
        let r = parse("3d6!").unwrap();
        assert_eq!(term(&r, 0).mods, vec![TermMod::Explode(None)]);

        let r = parse("d10!>8").unwrap();
        assert_eq!(
            term(&r, 0).mods,
            vec![TermMod::Explode(Some(Compare::Gt(8)))]
        );

        let r = parse("d6!=6").unwrap();
        assert_eq!(
            term(&r, 0).mods,
            vec![TermMod::Explode(Some(Compare::Eq(6)))]
        );
    }

    #[test]
    fn multiply() {
        let r = parse("4d6*2").unwrap();
        assert_eq!(term(&r, 0).mods, vec![TermMod::Mul(2)]);

        // Multiply binds to its term, not the whole roll.
        let r = parse("3d6*2+d8").unwrap();
        assert_eq!(term(&r, 0).mods, vec![TermMod::Mul(2)]);
        assert!(term(&r, 1).mods.is_empty());
    }

    #[test]
    fn stacked_modifiers() {
        // explode, then keep the best 3 of whatever results, then double.
        let r = parse("4d6!kh3*2").unwrap();
        assert_eq!(
            term(&r, 0).mods,
            vec![
                TermMod::Explode(None),
                TermMod::KeepHigh(3),
                TermMod::Mul(2)
            ]
        );
    }

    #[test]
    fn stakes_target() {
        // Meet-or-beat: `vs` and `>` are the same, with or without spaces, any
        // case. `vs` is just the word alias for `>`.
        for expr in [
            "d20+5 vs 15",
            "d20+5vs15",
            "d20+5 VS 15",
            "d20+5 > 15",
            "d20+5>15",
        ] {
            let r = parse(expr).unwrap();
            assert_eq!(
                r.stake,
                Some(Stake {
                    target: 15,
                    goal: Goal::Over
                }),
                "{expr}"
            );
            assert_eq!(r.modifier, 5, "{expr}");
        }
        // No stakes: the stake is absent.
        assert_eq!(parse("3d6").unwrap().stake, None);
        // The target may be negative (weird, but well-defined).
        assert_eq!(
            parse("d6 vs -2").unwrap().stake,
            Some(Stake {
                target: -2,
                goal: Goal::Over
            })
        );
    }

    #[test]
    fn stakes_roll_under() {
        // `<` is roll-under, spaced or not, and the target still ends the roll.
        for expr in ["d20 < 15", "d20<15", "2d6<7"] {
            let goal = parse(expr).unwrap().stake.map(|s| s.goal);
            assert_eq!(goal, Some(Goal::Under), "{expr}");
        }
        assert_eq!(
            parse("d20 < 15").unwrap().stake,
            Some(Stake {
                target: 15,
                goal: Goal::Under
            })
        );
        // Roll-under against a negative target still parses (the `-` is the
        // number's sign, not another operator).
        assert_eq!(
            parse("d6 < -2").unwrap().stake,
            Some(Stake {
                target: -2,
                goal: Goal::Under
            })
        );
    }

    #[test]
    fn stakes_errors() {
        assert!(parse("d20 vs").is_err(), "vs needs a number");
        assert!(parse("d20 >").is_err(), "'>' needs a number");
        assert!(parse("d20 <").is_err(), "roll-under still needs a number");
        assert!(parse("d20 vs 10 vs 12").is_err(), "one target only");
        assert!(parse("d20 > 10 < 5").is_err(), "one target only");
        assert!(parse("vs 15").is_err(), "stakes with no dice is not a roll");
        assert!(
            parse("< 15").is_err(),
            "roll-under with no dice is not a roll"
        );
        // The target ends the roll: trailing terms are an error, not a bonus
        // die quietly added to the roller's own pool.
        assert!(
            parse("d20 vs 4d6").is_err(),
            "dice after the target must not parse"
        );
        assert!(
            parse("d20 vs 15 + 5").is_err(),
            "modifiers after the target must not parse"
        );
        // A magnitude that fits u32 but not i32 must error, not wrap negative.
        assert!(
            parse("d20 vs 3000000000").is_err(),
            "an oversized target must not wrap"
        );
        assert!(parse("d20 vs -3000000000").is_err());
        // i32::MIN's magnitude is the one negative that must not panic.
        assert!(
            parse("d20 vs -2147483648").is_ok(),
            "i32::MIN is a valid (absurd) target"
        );
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("d").is_err());
        assert!(parse("5").is_err()); // modifier with no dice
        assert!(parse("d0").is_err());
        assert!(parse("xyz").is_err());
        assert!(parse("d1!").is_err()); // can't explode a d1
        assert!(parse("4d6*").is_err()); // multiply needs a number
        assert!(parse("d6!>").is_err()); // comparison needs a number
    }
}
