use rand::Rng;

/// 生成 16 字符 hex token，足够抗 LAN 内枚举。
pub fn generate_token() -> String {
    let bytes: [u8; 8] = rand::rng().random();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}
