//! Small shared helpers.

/// `"Fix login: OAuth redirect!"` -> `"fix-login-oauth-redirect"` (max 40 chars).
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "task".into()
    } else {
        out
    }
}

/// Branch name for a ticket: `boba/T-<n>-<slug>`.
pub fn ticket_branch(number: i64, title: &str) -> String {
    format!("boba/T-{number}-{}", slugify(title))
}

/// Tiny deterministic PRNG (SplitMix64): enough for demo variety, no extra dependency.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [lo, hi] (inclusive). Requires lo <= hi.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo + 1)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.f64() < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[(self.next_u64() % items.len() as u64) as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(
            slugify("Fix login: OAuth redirect!"),
            "fix-login-oauth-redirect"
        );
        assert_eq!(slugify("  ¿¿  "), "task");
        assert!(slugify(&"a".repeat(100)).len() <= 40);
        assert_eq!(ticket_branch(7, "Add dark mode"), "boba/T-7-add-dark-mode");
    }

    #[test]
    fn rng_is_deterministic_and_bounded() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            let x = a.f64();
            assert_eq!(x, b.f64());
            assert!((0.0..1.0).contains(&x));
            let r = a.range(3, 5);
            b.range(3, 5);
            assert!((3..=5).contains(&r));
        }
    }
}
