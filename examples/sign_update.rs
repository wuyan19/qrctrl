//! 给更新资产 / manifest 签名（CI 的 release.yml manifest job 与本地手动补签共用）。
//!
//! 用法：
//! ```shell
//! # 本地（默认从 .sign/ 取私钥和口令，无需任何选项）
//! cargo run --release --example sign_update -- <待签名文件>
//!
//! # CI（私钥从 GitHub Secrets 落盘、口令走环境注入，显式传参）
//! cargo run --release --example sign_update -- \
//!   --key <私钥文件> --password <口令> <待签名文件>
//! ```
//! 私钥 / 口令解析顺序：`--key` / `--password` 显式 → `--password-file` 显式 →
//! 默认 `.sign/minisign.key` + `.sign/minisign.pwd`（keygen_update 的产物布局）。
//!
//! 签名（minisign .sig 格式全文）打到 stdout，调用方捕获后嵌入 update-manifest.json。
//!
//! 本工具只进 dev 环境（minisign crate 是 dev-dependency），客户端运行时
//! 用的是零依赖的 minisign-verify——签名能力和验证能力分离，release binary
//! 不携带私钥处理代码。

use std::io::Read;

const DEFAULT_KEY: &str = ".sign/minisign.key";
const DEFAULT_PWD_FILE: &str = ".sign/minisign.pwd";

fn main() {
    let mut key_path: Option<String> = None;
    let mut password: Option<String> = None;
    let mut password_file: Option<String> = None;
    let mut target: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // 两种形式都认：--key=<v> 和 --key <v>（后者消费下一个参数）
            a if a.starts_with("--key=") => key_path = Some(a.trim_start_matches("--key=").into()),
            "--key" => key_path = args.next(),
            a if a.starts_with("--password=") => {
                password = Some(a.trim_start_matches("--password=").into())
            }
            "--password" => password = args.next(),
            a if a.starts_with("--password-file=") => {
                password_file = Some(a.trim_start_matches("--password-file=").into())
            }
            "--password-file" => password_file = args.next(),
            a if !a.starts_with("--") && target.is_none() => target = Some(a.into()),
            other => {
                eprintln!("未知参数：{}", other);
                std::process::exit(2);
            }
        }
    }
    let key_path = key_path.unwrap_or_else(|| DEFAULT_KEY.to_string());
    let target = target.unwrap_or_else(|| {
        eprintln!("缺少待签名文件参数");
        std::process::exit(2);
    });

    // 口令三级解析：显式 --password > --password-file > 默认 .sign/minisign.pwd。
    // 注意 minisign 的 into_secret_key(None) 会转交互式 stdin 询问（CI 卡死），
    // 所以这里必须解析出一个非 None 的值，解析不到就报错而不是交给库去问。
    let password = if let Some(p) = password {
        p
    } else {
        let pwd_file = password_file.unwrap_or_else(|| DEFAULT_PWD_FILE.to_string());
        match std::fs::read_to_string(&pwd_file) {
            Ok(p) => p.trim().to_string(),
            Err(e) => {
                eprintln!(
                    "缺少口令：--password=<口令> 未给，且读取 {} 失败：{}",
                    pwd_file, e
                );
                std::process::exit(2);
            }
        }
    };

    let key_str = std::fs::read_to_string(&key_path)
        .unwrap_or_else(|e| panic!("读取私钥 {} 失败：{}", key_path, e));
    let sk_box = minisign::SecretKeyBox::from_string(&key_str).expect("私钥文件格式非法");
    let sk = sk_box
        .into_secret_key(Some(password))
        .expect("私钥口令错误或私钥损坏");

    let mut data = Vec::new();
    std::fs::File::open(&target)
        .and_then(|mut f| f.read_to_end(&mut data))
        .unwrap_or_else(|e| panic!("读取 {} 失败：{}", target, e));

    // trusted comment 带文件名：验签端校验通过后可追溯「这个签名属于哪个文件」，
    // 防止把 A 资产的签名挪用到 B 资产上（minisign 的 trusted comment 由
    // global signature 保护，篡改会连带验签失败）。
    let file_name = std::path::Path::new(&target)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let trusted = format!("qrctrl-update\tfile:{}", file_name);
    // 第一个参数（pk）传 None：签名里 key id 留空，不影响验签（公钥在客户端编译期嵌入）。
    let signature_box = minisign::sign(None, &sk, data.as_slice(), Some(&trusted), None)
        .expect("签名失败");
    print!("{}", signature_box.to_string());
}
