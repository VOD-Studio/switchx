//! Random identifiers for records, requests, and temporary paths.

pub fn new_id() -> Result<String, String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "无法生成上游 ID")?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
