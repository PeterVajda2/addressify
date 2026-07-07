use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct StructuredAddress {
    pub country_code: String,
    pub admin_area: Option<String>,
    pub locality: Option<String>,
    pub dependent_locality: Option<String>,
    pub thoroughfare: Option<String>,
    pub premise: Option<String>,
    pub premise_type: Option<String>,
    pub subpremise: Option<String>,
    pub postal_code: Option<String>,
    pub full_address: String,
}

#[derive(Debug, Clone)]
pub struct Address {
    pub country_code: String,
    pub admin_area: Option<String>,
    pub locality: Option<String>,
    pub dependent_locality: Option<String>,
    pub thoroughfare: Option<String>,
    pub premise: Option<String>,
    pub premise_type: Option<String>,
    pub subpremise: Option<String>,
    pub postal_code: Option<String>,
    pub full_address: String,
    pub search_text: String,
}

impl Address {
    pub fn from_parts(parts: StructuredAddress, search_text: impl Into<String>) -> Self {
        Self {
            country_code: parts.country_code,
            admin_area: parts.admin_area,
            locality: parts.locality,
            dependent_locality: parts.dependent_locality,
            thoroughfare: parts.thoroughfare,
            premise: parts.premise,
            premise_type: parts.premise_type,
            subpremise: parts.subpremise,
            postal_code: parts.postal_code,
            full_address: parts.full_address,
            search_text: search_text.into(),
        }
    }

    pub fn formatted(&self) -> &str {
        &self.full_address
    }

    pub fn structured(&self) -> StructuredAddress {
        StructuredAddress {
            country_code: self.country_code.clone(),
            admin_area: self.admin_area.clone(),
            locality: self.locality.clone(),
            dependent_locality: self.dependent_locality.clone(),
            thoroughfare: self.thoroughfare.clone(),
            premise: self.premise.clone(),
            premise_type: self.premise_type.clone(),
            subpremise: self.subpremise.clone(),
            postal_code: self.postal_code.clone(),
            full_address: self.full_address.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub formatted: String,
    pub score: f32,
    pub country_code: String,
    pub address: StructuredAddress,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatted_address_is_the_database_address() {
        let address = Address::from_parts(
            StructuredAddress {
                country_code: String::from("SK"),
                admin_area: None,
                locality: Some(String::from("Kosice")),
                dependent_locality: None,
                thoroughfare: Some(String::from("Hlavna")),
                premise: Some(String::from("68")),
                premise_type: None,
                subpremise: None,
                postal_code: Some(String::from("040 01")),
                full_address: String::from("Hlavná 68, Košice, 040 01, SK"),
            },
            "hlavna 68 kosice 040 01 sk",
        );

        assert_eq!(address.formatted(), "Hlavná 68, Košice, 040 01, SK");
    }
}
