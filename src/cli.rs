//! Command-line interface and the one-shot (non-animated) output path.
//!
//! When `roll` is given an expression together with an output flag — or when its
//! stdout isn't a terminal — it skips the bouncing-dice TUI entirely, evaluates
//! the roll once, prints a result, and exits. This is what makes `roll` usable in
//! scripts and pipelines. Three output shapes: a bare total (default), a verbose
//! breakdown (`-v`), and machine-readable JSON (`--json`).
//!
//! A staked roll (`d20+5 vs 15`) under an explicit `-p`/`-v` also *exits* with
//! the verdict — 0 on success, 1 on failure — so a shell script can branch on
//! a saving throw: `roll -p d20+4 vs 14 && echo "the potion works"`. The
//! implicit piped-stdout mode and `--json` always exit 0 on clean output:
//! `roll d20 vs 15 | tee log` in a `set -e` script must not abort just
//! because the die came up short, and JSON consumers read `success` instead.

use std::io::{self, Write};

use clap::Parser;
use rand::rngs::StdRng;
use rand::SeedableRng;

use crate::app::{evaluate, Outcome};
use crate::parse;

/// A terminal dice roller with bouncing dice.
///
/// With no expression it opens the interactive animation. Given an expression
/// and an output flag (or a piped stdout) it prints a result and exits.
#[derive(Parser, Debug)]
#[command(name = "roll", version, about, long_about = None)]
pub struct Cli {
    /// Dice expression, e.g. `3d6`, `2d20kh1`, `4d6dl1+2`. Multiple words are
    /// joined, so `roll d6 + d8` and `roll "d6+d8"` are equivalent.
    #[arg(value_name = "EXPRESSION", trailing_var_arg = true)]
    pub expr: Vec<String>,

    /// Print the result and exit instead of animating (implied when stdout is
    /// piped or redirected).
    #[arg(short, long)]
    pub print: bool,

    /// Print a full breakdown — each die, with dropped and exploded marked —
    /// not just the total. Implies one-shot.
    #[arg(short, long)]
    pub verbose: bool,

    /// Emit the result as JSON (per-term, per-die, totals). Implies one-shot.
    #[arg(long)]
    pub json: bool,

    /// Seed the RNG for a reproducible roll (works in both modes).
    #[arg(long, value_name = "N")]
    pub seed: Option<u64>,

    /// Start the interactive mode muted (Ctrl-M toggles sound at runtime).
    #[arg(long)]
    pub mute: bool,
}

impl Cli {
    /// The expression as a single trimmed string (the joined positional args).
    pub fn expression(&self) -> String {
        self.expr.join(" ").trim().to_string()
    }
}

/// Evaluate `expr` once and print it per the chosen format, then return. Parse
/// errors are written to stderr and surfaced as a non-zero exit.
pub fn run_one_shot(cli: &Cli, expr: &str) -> io::Result<()> {
    let roll = match parse::parse(expr) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("roll: {e}");
            std::process::exit(2);
        }
    };

    // Seeded when asked, else fresh entropy. The same evaluator the TUI's roll
    // logic mirrors, so printed and animated results follow identical rules.
    let mut rng = match cli.seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_entropy(),
    };
    let outcome = evaluate(expr, &roll, &mut rng);

    let stdout = io::stdout();
    let mut out = stdout.lock();
    if cli.json {
        // Pretty JSON so a human can read it too; scripts parse either way.
        let json = serde_json::to_string_pretty(&outcome).expect("Outcome serializes");
        writeln!(out, "{json}")?;
    } else if cli.verbose {
        write!(out, "{}", format_verbose(&outcome))?;
    } else {
        writeln!(out, "{}", outcome.total)?;
    }

    // A staked roll reports its verdict through the exit code (0 = success,
    // 1 = failure) so scripts can branch on the check itself — but only when
    // the caller explicitly asked for `-p`/`-v`. The automatic non-tty
    // one-shot (`roll d20 vs 15 | tee`) and `--json` keep exit 0, so piping
    // output can never read as the program failing.
    if (cli.print || cli.verbose) && outcome.success == Some(false) {
        out.flush()?;
        std::process::exit(1);
    }
    Ok(())
}

/// A human-readable multi-line breakdown: one line per term showing every die
/// (dropped dice in [brackets], exploded dice marked `!`), then the grand total.
pub fn format_verbose(o: &Outcome) -> String {
    use std::fmt::Write;
    let mut s = String::new();

    for term in &o.terms {
        let faces: Vec<String> = term
            .dice
            .iter()
            .map(|d| {
                let mut f = d.value.to_string();
                if d.exploded {
                    f.push('!'); // spawned by an explosion
                }
                if !d.kept {
                    f = format!("[{f}]"); // dropped by keep/drop
                }
                f
            })
            .collect();

        let _ = write!(s, "  {:<10} {}", term.notation, faces.join(" "));
        if term.multiplier != 1 {
            let _ = write!(s, "  ×{}", term.multiplier);
        }
        let _ = writeln!(s, "  = {}", term.subtotal);
    }

    if o.modifier != 0 {
        let sign = if o.modifier > 0 { "+" } else { "−" };
        let _ = writeln!(s, "  {:<10} {sign}{}", "modifier", o.modifier.abs());
    }

    let _ = writeln!(s, "  total      {}", o.total);

    // The staked verdict, spelled out with the same wording as the TUI chip
    // (the exit code carries it for scripts).
    if let (Some(target), Some(success), Some(margin)) = (o.target, o.success, o.margin) {
        let verdict = crate::app::verdict_text(success, margin).to_lowercase();
        let _ = writeln!(s, "  vs {target:<7} {verdict}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(expr: &str, seed: u64) -> Outcome {
        let roll = parse::parse(expr).unwrap();
        let mut rng = StdRng::seed_from_u64(seed);
        evaluate(expr, &roll, &mut rng)
    }

    #[test]
    fn seeded_evaluation_is_deterministic() {
        let a = outcome("4d6dl1+2", 42);
        let b = outcome("4d6dl1+2", 42);
        assert_eq!(a.total, b.total);
        let av: Vec<u32> = a.terms[0].dice.iter().map(|d| d.value).collect();
        let bv: Vec<u32> = b.terms[0].dice.iter().map(|d| d.value).collect();
        assert_eq!(av, bv, "same seed must reproduce the same dice");
    }

    #[test]
    fn total_matches_kept_dice_and_modifier() {
        let o = outcome("4d6dl1+2", 7);
        // Four dice rolled, exactly one dropped.
        assert_eq!(o.terms[0].dice.len(), 4);
        assert_eq!(o.terms[0].dice.iter().filter(|d| !d.kept).count(), 1);
        // Total = sum of kept dice + modifier.
        let kept: i32 = o.terms[0]
            .dice
            .iter()
            .filter(|d| d.kept)
            .map(|d| d.value as i32)
            .sum();
        assert_eq!(o.total, kept + 2);
        assert_eq!(o.modifier, 2);
    }

    #[test]
    fn multiply_is_reflected_in_subtotal() {
        let o = outcome("3d6*2+d8", 3);
        let t0 = &o.terms[0];
        let raw: i32 = t0
            .dice
            .iter()
            .filter(|d| d.kept)
            .map(|d| d.value as i32)
            .sum();
        assert_eq!(t0.multiplier, 2);
        assert_eq!(t0.subtotal, raw * 2);
    }

    #[test]
    fn exploded_dice_are_flagged() {
        // Find a seed that explodes a d6 and check the spawned die is marked.
        for seed in 0..200 {
            let o = outcome("6d6!", seed);
            if o.terms[0].dice.iter().any(|d| d.exploded) {
                // Every exploded die is kept (explosions always count).
                assert!(o.terms[0]
                    .dice
                    .iter()
                    .filter(|d| d.exploded)
                    .all(|d| d.kept));
                return;
            }
        }
        panic!("no seed produced an explosion");
    }

    #[test]
    fn json_has_the_expected_shape() {
        let o = outcome("2d20kh1", 1);
        let v: serde_json::Value = serde_json::to_value(&o).unwrap();
        assert!(v["total"].is_number());
        assert_eq!(v["terms"][0]["notation"], "2d20kh1");
        assert_eq!(v["terms"][0]["dice"].as_array().unwrap().len(), 2);
        // Every die object carries value/kept/exploded.
        let die = &v["terms"][0]["dice"][0];
        assert!(die["value"].is_number());
        assert!(die["kept"].is_boolean());
        assert!(die["exploded"].is_boolean());
        // Crit/fumble ride along so JSON consumers never re-derive the rule.
        assert!(die["crit"].is_boolean());
        assert!(die["fumble"].is_boolean());
    }

    #[test]
    fn json_crits_match_the_arena_rule() {
        // Any die type crits on its max: find a d6 that rolled a 6 and a 1.
        for seed in 0..300 {
            let o = outcome("4d6", seed);
            for d in &o.terms[0].dice {
                assert_eq!(d.crit, d.kept && d.value == 6, "seed {seed}");
                assert_eq!(d.fumble, d.kept && d.value == 1, "seed {seed}");
            }
        }
        // A dropped max die is not a crit.
        for seed in 0..500 {
            let o = outcome("2d20kl1", seed);
            if let Some(d) = o.terms[0].dice.iter().find(|d| !d.kept && d.value == 20) {
                assert!(!d.crit, "seed {seed}: a dropped 20 must not be a crit");
                return;
            }
        }
        panic!("no seed dropped a 20 under disadvantage");
    }

    #[test]
    fn verbose_marks_dropped_and_shows_total() {
        let o = outcome("4d6dl1", 7);
        let text = format_verbose(&o);
        assert!(text.contains("4d6dl1"), "term notation missing");
        assert!(text.contains('['), "dropped die not bracketed");
        assert!(text.contains("total"), "total line missing");
    }

    #[test]
    fn expression_joins_args() {
        let cli = Cli::parse_from(["roll", "d6", "+", "d8"]);
        assert_eq!(cli.expression(), "d6 + d8");
        // Stakes survive the join too: `roll d20 vs 15` needs no quotes.
        let cli = Cli::parse_from(["roll", "d20", "vs", "15"]);
        assert_eq!(cli.expression(), "d20 vs 15");
    }

    #[test]
    fn staked_outcome_carries_the_verdict() {
        // d20+5 vs 15 for a seed spread: verdict always matches the total.
        for seed in 0..50 {
            let o = outcome("d20+5 vs 15", seed);
            assert_eq!(o.target, Some(15));
            assert_eq!(o.margin, Some(o.total - 15));
            assert_eq!(o.success, Some(o.total >= 15), "seed {seed}");
        }
        // Unstaked rolls carry nothing.
        let o = outcome("3d6", 1);
        assert_eq!((o.target, o.success, o.margin), (None, None, None));
    }

    #[test]
    fn staked_json_and_verbose_show_the_verdict() {
        let o = outcome("d20 vs 10", 3);
        let v: serde_json::Value = serde_json::to_value(&o).unwrap();
        assert_eq!(v["target"], 10);
        assert!(v["success"].is_boolean());
        assert!(v["margin"].is_number());
        let text = format_verbose(&o);
        assert!(
            text.contains("vs 10"),
            "verbose verdict line missing:\n{text}"
        );
        assert!(
            text.contains("success") || text.contains("fail"),
            "no verdict wording:\n{text}"
        );

        // Unstaked JSON omits the stake keys entirely.
        let v: serde_json::Value = serde_json::to_value(outcome("3d6", 1)).unwrap();
        assert!(v.get("target").is_none());
    }
}
