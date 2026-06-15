use rand::Rng;

/// 生成 16 字符 hex token，足够抗 LAN 内枚举。
pub fn generate_token() -> String {
    let bytes: [u8; 8] = rand::rng().random();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// 自定义 token 的长度上下限。
pub const MIN_TOKEN_LEN: usize = 4;
pub const MAX_TOKEN_LEN: usize = 64;

/// 校验用户通过 `--token` 传入的自定义 token。
///
/// 规则：4-64 个字符，仅允许 ASCII 字母数字。字母数字保证 URL 安全
/// （无需 percent-encode），也避免任何注入风险（token 会进 WebSocket
/// query string 和 HTTP 路由）。
pub fn validate_token(s: &str) -> Result<(), String> {
    if s.len() < MIN_TOKEN_LEN {
        return Err(format!("token 至少 {} 个字符", MIN_TOKEN_LEN));
    }
    if s.len() > MAX_TOKEN_LEN {
        return Err(format!("token 至多 {} 个字符", MAX_TOKEN_LEN));
    }
    if !s.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("token 只能包含 ASCII 字母和数字".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_is_hex_and_16_chars() {
        let t = generate_token();
        assert_eq!(t.len(), 16);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_tokens_are_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a, b);
    }

    #[test]
    fn validate_accepts_min_length() {
        assert!(validate_token("abcd").is_ok());
    }

    #[test]
    fn validate_accepts_max_length() {
        let t = "a".repeat(MAX_TOKEN_LEN);
        assert!(validate_token(&t).is_ok());
    }

    #[test]
    fn validate_rejects_too_short() {
        assert!(validate_token("abc").is_err());
    }

    #[test]
    fn validate_rejects_too_long() {
        let t = "a".repeat(MAX_TOKEN_LEN + 1);
        assert!(validate_token(&t).is_err());
    }

    #[test]
    fn validate_rejects_non_alphanumeric() {
        // 含特殊字符（即使在 URL 里安全）也拒绝，保持规则简单
        assert!(validate_token("ab-cd").is_err());
        assert!(validate_token("ab_cd").is_err());
        assert!(validate_token("ab.cd").is_err());
        assert!(validate_token("ab cd").is_err());
    }

    #[test]
    fn validate_rejects_non_ascii() {
        assert!(validate_token("你好你好").is_err());
    }

    #[test]
    fn validate_accepts_mixed_case_and_digits() {
        assert!(validate_token("AbCd1234").is_ok());
        assert!(validate_token("0123456789").is_ok());
        assert!(validate_token("XYZxyz").is_ok());
    }
}
