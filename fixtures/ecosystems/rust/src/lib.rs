//! A fixture: one dependency, one test.

/// `n` in decimal
pub fn decimal(n: u64) -> String {
    itoa::Buffer::new().format(n).to_owned()
}

#[cfg(test)]
mod tests {
    use super::decimal;

    #[test]
    fn writes_a_number_in_decimal() {
        assert_eq!(decimal(1024), "1024");
    }
}
