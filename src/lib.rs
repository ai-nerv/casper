//! casper.

/// Returns this crate's display name.
pub fn name() -> &'static str {
    "casper"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_name() {
        assert_eq!(name(), "casper");
    }
}
