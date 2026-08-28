//! 验证 update-manifest.json：信封验签 + 内层解析 + 摘要打印。
//!
//! 用途：
//! - **本地端到端自检**：CI 逻辑（python 拼 manifest + sign_update 签名）在本地
//!   模拟后，用它确认客户端路径能验过；
//! - **发布后 smoke**：release workflow 在 upload manifest 后下载回来跑一遍，
//!   防止「publish 了但 manifest 签名/结构是坏的」这类静默故障。
//!
//! 用法：
//! ```shell
//! cargo run --example verify_manifest -- <manifest 路径>   # 默认 update-manifest.json
//! ```
//!
//! 通过 `#[path]` 直接引入 src/update.rs（该模块无 crate:: 依赖，可独立编译），
//! 走的就是客户端同一条验证代码，不是复制品。

// example 只用解析/验签部分，Updater 状态机等在此编译单元里未被引用
#[allow(dead_code)]
#[path = "../src/update.rs"]
mod update;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "update-manifest.json".to_string());
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("读取 {} 失败：{}", path, e);
            std::process::exit(1);
        }
    };
    match update::parse_signed_manifest(&body) {
        Ok(m) => {
            println!("✓ 签名验证通过");
            println!("  version:    {}", m.version);
            println!("  notes_url:  {}", m.notes_url);
            for (key, a) in &m.platforms {
                println!("  {:<18} size={:<10} sha256={}", key, a.size, &a.sha256[..12.min(a.sha256.len())]);
            }
        }
        Err(e) => {
            eprintln!("✗ 验证失败：{}", e);
            std::process::exit(1);
        }
    }
}
