pub fn normalize_text(input: &str) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut last_was_space = false;

    for c in input.to_lowercase().chars() {
        if let Some(folded) = fold_alphanumeric(c) {
            normalized.push(folded);
            last_was_space = false;
        } else if c.is_whitespace() || matches!(c, ',' | '.' | '-' | '/' | '#' | ':' | ';') {
            if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        }
    }

    normalized.trim().to_string()
}

fn fold_alphanumeric(c: char) -> Option<char> {
    let folded = match c {
        'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ą' => 'a',
        'æ' => 'a',
        'ç' | 'č' | 'ć' => 'c',
        'ď' => 'd',
        'é' | 'ě' | 'è' | 'ê' | 'ë' | 'ę' => 'e',
        'í' | 'ì' | 'î' | 'ï' => 'i',
        'ľ' | 'ĺ' | 'ł' => 'l',
        'ñ' | 'ň' => 'n',
        'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ø' => 'o',
        'ř' => 'r',
        'š' | 'ś' => 's',
        'ť' => 't',
        'ú' | 'ů' | 'ù' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        'ž' | 'ź' | 'ż' => 'z',
        _ if c.is_ascii_alphanumeric() => c,
        _ => return None,
    };

    Some(folded)
}

#[cfg(test)]
mod tests {
    use super::normalize_text;

    #[test]
    fn normalizes_diacritics_and_punctuation() {
        assert_eq!(
            normalize_text("Avenue de France 123, Stiring-Wendel"),
            "avenue de france 123 stiring wendel"
        );
    }
}
