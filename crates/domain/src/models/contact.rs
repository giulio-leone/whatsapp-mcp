use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContactId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    pub id: ContactId,
    pub name: Option<String>,
    pub push_name: Option<String>,
    pub formatted_number: String,
    pub is_business: bool,
}
