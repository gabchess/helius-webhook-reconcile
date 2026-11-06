//! Chunk 0 placeholder. Product code starts in chunk 1.

/// Returns the greeting that the hello test checks.
pub fn hello() -> &'static str {
    "webhook-reconcile"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_names_the_crate() {
        assert_eq!(hello(), "webhook-reconcile");
    }
}
