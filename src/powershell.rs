//! Quoting values for PowerShell scripts.

/// The characters PowerShell accepts as a single quote: the ASCII one and
/// the typographic ‘ ’ ‚ ‛, all of which may appear in file names.
const SINGLE_QUOTES: [char; 5] = ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'];

/// `value` as a PowerShell single-quoted string literal. Inside one, a
/// doubled quote of any of these kinds stands for itself, so doubling
/// every one keeps the value from ending the string early.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if SINGLE_QUOTES.contains(&c) {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits `script` the way PowerShell's tokenizer reads a single-quoted
    /// string starting at its first character, returning the string's value
    /// and whatever follows it.
    fn read_literal(script: &str) -> (String, String) {
        let mut chars = script.chars().peekable();
        assert!(SINGLE_QUOTES.contains(&chars.next().unwrap()));
        let mut value = String::new();
        while let Some(c) = chars.next() {
            if SINGLE_QUOTES.contains(&c) {
                if chars.peek().is_some_and(|n| SINGLE_QUOTES.contains(n)) {
                    value.push(chars.next().unwrap());
                    continue;
                }
                return (value, chars.collect());
            }
            value.push(c);
        }
        panic!("unterminated literal");
    }

    #[test]
    fn typographic_quotes_in_file_names_cannot_end_the_string() {
        for name in [
            "C:\\a'; calc; '.heic",
            "C:\\a\u{2019}; calc; \u{2019}.heic",
            "C:\\a\u{2018}; calc; #.heic",
            "C:\\a\u{201A}; calc; #.heic",
            "C:\\a\u{201B}; calc; #.heic",
            "C:\\Tom\u{2019}s photos\\IMG_0001.HEIC",
        ] {
            let (value, rest) = read_literal(&format!("{}; Write-Output done", literal(name)));
            assert_eq!(value, name);
            assert_eq!(rest, "; Write-Output done");
        }
    }

    #[test]
    fn ordinary_paths_are_simply_quoted() {
        assert_eq!(literal("D:\\"), "'D:\\'");
        assert_eq!(literal(""), "''");
    }
}
