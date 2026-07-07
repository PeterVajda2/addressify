#[derive(Debug, Clone, Default)]
pub struct ParsedPremise {
    pub house_number: Option<String>,
    pub unit: Option<String>,
    pub house_number_type: Option<String>,
}

pub fn normalize_address_parts(
    country_code: &str,
    premise: Option<&str>,
    subpremise: Option<&str>,
) -> ParsedPremise {
    let raw_premise = collapse_spaces(premise);
    let raw_subpremise = collapse_spaces(subpremise);

    let (without_prefix, house_number_type) =
        strip_house_number_prefix(country_code, raw_premise.as_deref());
    let (house_number, parsed_unit) =
        split_house_and_unit(without_prefix.as_deref()).unwrap_or((without_prefix, None));

    let unit = raw_subpremise.or(parsed_unit);

    ParsedPremise {
        house_number,
        unit,
        house_number_type,
    }
}

pub fn clean_thoroughfare(input: Option<&str>) -> Option<String> {
    collapse_spaces(input)
}

pub fn format_display_address(
    country_code: &str,
    thoroughfare: Option<&str>,
    house_number: Option<&str>,
    unit: Option<&str>,
    locality: Option<&str>,
    dependent_locality: Option<&str>,
    admin_area: Option<&str>,
    postal_code: Option<&str>,
) -> String {
    let street = collapse_spaces(thoroughfare);
    let house = collapse_spaces(house_number);
    let unit = collapse_spaces(unit);
    let city = collapse_spaces(locality);
    let district = collapse_spaces(dependent_locality);
    let region = collapse_spaces(admin_area);
    let postal = collapse_spaces(postal_code);

    let mut parts: Vec<String> = Vec::new();

    match country_code {
        "CZ" | "SK" => {
            if let Some(street) = street {
                let first = if let Some(house) = house {
                    format!("{street} {}", join_house_unit(&house, unit.as_deref()))
                } else {
                    street
                };
                parts.push(first);
            } else if let Some(house) = house {
                parts.push(join_house_unit(&house, unit.as_deref()));
            }

            if let Some(value) = district {
                parts.push(value);
            }

            match (postal, city) {
                (Some(postal), Some(city)) => parts.push(format!("{postal} {city}")),
                (None, Some(city)) => parts.push(city),
                (Some(postal), None) => parts.push(postal),
                (None, None) => {}
            }
        }
        _ => {
            match (street, house) {
                (Some(street), Some(house)) => parts.push(format!(
                    "{street} {}",
                    join_house_unit(&house, unit.as_deref())
                )),
                (Some(street), None) => parts.push(street),
                (None, Some(house)) => parts.push(join_house_unit(&house, unit.as_deref())),
                (None, None) => {}
            }

            if let Some(value) = district {
                parts.push(value);
            }
            if let Some(value) = city {
                parts.push(value);
            }

            match (postal, region) {
                (Some(postal), Some(region)) => {
                    parts.push(region);
                    parts.push(postal);
                }
                (Some(postal), None) => parts.push(postal),
                (None, Some(region)) => parts.push(region),
                (None, None) => {}
            }
        }
    }

    parts.push(country_code.to_string());
    parts.join(", ")
}

fn join_house_unit(house_number: &str, unit: Option<&str>) -> String {
    match unit {
        Some(unit) if !unit.is_empty() => format!("{house_number}/{unit}"),
        _ => house_number.to_string(),
    }
}

fn strip_house_number_prefix(
    country_code: &str,
    premise: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Some(premise) = collapse_spaces(premise) else {
        return (None, None);
    };

    let mut tokens: Vec<String> = premise
        .split_whitespace()
        .map(|part| {
            part.trim_matches(|c: char| {
                c == '.' || c == ',' || c == ';' || c == ':' || c == '(' || c == ')' || c == '#'
            })
            .to_string()
        })
        .filter(|part| !part.is_empty())
        .collect();

    if tokens.is_empty() {
        return (None, None);
    }

    if matches!(country_code, "CZ" | "SK") {
        let first = tokens[0].to_lowercase();
        let second = tokens.get(1).map(|v| v.to_lowercase());

        let (consume, marker) = match (first.as_str(), second.as_deref()) {
            ("č", Some("p")) | ("c", Some("p")) => (2, Some("conscription")),
            ("č", Some("ev")) | ("c", Some("ev")) => (2, Some("evidence")),
            ("čp" | "cp" | "č.p" | "c.p", _) => (1, Some("conscription")),
            ("čev" | "cev" | "č.ev" | "c.ev" | "ev", _) => (1, Some("evidence")),
            ("č", _) => (1, None),
            _ => (0, None),
        };

        if consume > 0 {
            tokens = tokens.into_iter().skip(consume).collect();
        }

        return (joined_tokens(tokens), marker.map(ToString::to_string));
    }

    (joined_tokens(tokens), None)
}

fn split_house_and_unit(value: Option<&str>) -> Option<(Option<String>, Option<String>)> {
    let value = collapse_spaces(value)?;
    let compact = value.replace(' ', "");

    if let Some((house, unit)) = split_number_pair(&compact, '.') {
        return Some((Some(house), Some(unit)));
    }
    if let Some((house, unit)) = split_number_pair(&compact, '/') {
        return Some((Some(house), Some(unit)));
    }

    Some((Some(compact), None))
}

fn split_number_pair(value: &str, sep: char) -> Option<(String, String)> {
    let (left, right) = value.split_once(sep)?;
    if left.is_empty() || right.is_empty() {
        return None;
    }

    if is_house_token(left) && is_house_token(right) {
        Some((left.to_string(), right.to_string()))
    } else {
        None
    }
}

fn is_house_token(value: &str) -> bool {
    value.chars().all(|c| c.is_ascii_alphanumeric())
}

fn joined_tokens(tokens: Vec<String>) -> Option<String> {
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" "))
    }
}

fn collapse_spaces(input: Option<&str>) -> Option<String> {
    let raw = input?;
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        None
    } else {
        Some(collapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::{format_display_address, normalize_address_parts};

    #[test]
    fn parses_cz_conscription_marker() {
        let parsed = normalize_address_parts("CZ", Some("č.p. 508"), None);
        assert_eq!(parsed.house_number.as_deref(), Some("508"));
        assert_eq!(parsed.house_number_type.as_deref(), Some("conscription"));
    }

    #[test]
    fn formats_cz_without_street() {
        let rendered = format_display_address(
            "CZ",
            None,
            Some("508"),
            None,
            Some("Jaromerice"),
            None,
            None,
            Some("56944"),
        );
        assert_eq!(rendered, "508, 56944 Jaromerice, CZ");
    }
}
