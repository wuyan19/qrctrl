# 在线升级（self-update）调研

> 状态：**P0 + P1 已落地**（2026-08-28，见 src/update.rs + api.rs 的 `/api/update/*` + 配置页 Update 区块 + release.yml 的 manifest job；skip_version 跳过版本也已实现；P1 minisign 签名含密钥生成 / CI 签名 / 发布后 smoke 自检全链路）。**按用户要求：更新仅手动触发**——自动检查（24h 定时）、托盘「检查更新...」菜单入口、`auto_update_check` 配置项均已移除，不做恢复。剩余：P2 的手机端更新横幅与新版本 crash 回滚。全部落地后按惯例删除本文档。

## 0. 结论先行（TL;DR）

**推荐方案：GitHub Releases 自研薄更新层**——借鉴 Tauri updater 的「签名 manifest」模式，用小而精的 crate 组合：

- `ureq`（HTTP，默认 rustls，无 OpenSSL 依赖）拉 GitHub API + 下载资产
- `semver` 比较版本
- `self-replace`（mitsuhiko 出品，ruff/uv 生态在用）处理「替换正在运行的自身」
- `rust-minisign-verify`（Tauri 同款）做 Ed25519 验签
- `zip` 仅 macOS 端解 `.app.zip`

**排除项**：`self_update`（资产命名约定不匹配、无签名校验、不支持 .app bundle）、`cargo-dist` / `axoupdater`（Axo 公司 2025 年收摊，项目进入停维护状态，0.28.1 / 2025-07 是最后的实质版本）。

**分阶段**：P0 手动检查 + 替换 + 重启（sha256 完整性校验）→ P1 minisign 签名（CI 出签名 manifest）→ P2 自动检查 + 手机端提示。

---

## 1. 现状盘点（约束输入）

| 现状 | 对升级功能的影响 |
|---|---|
| 发布物：Windows 裸 `qrctrl-x86_64-windows.exe`、Linux 裸二进制、macOS 裸二进制 + `qrctrl.app.zip`（`ditto -c -k --keepParent` 格式） | Windows/Linux 直接覆盖单文件即可；macOS 需整包替换 .app（见 §4.2） |
| Release workflow `draft: true`，手动 publish | **坑：`GET /releases/latest` 不返回 draft**。忘点 publish 时所有客户端都查不到新版本——这是静默失败，需要在发布流程里固化「必须 publish」 |
| macOS 仅 ad-hoc 签名（`codesign --sign -`），无 Developer ID | 自更新反而比浏览器下载更顺：程序自己 HTTP 下载的文件**不带 quarantine 属性**（该属性由浏览器/LaunchServices 这类「自愿合作」的下载方打上），绕过 Gatekeeper 首次运行拦截 |
| 托盘常驻 app（Windows GUI subsystem / macOS LSUIElement），无单实例锁 | 重启时序是关键难点：新旧进程并发会触发 `probe_port` 自动 +1 滑到 8081，QR 码指向就变了（见 §4.1） |
| 已有 restart 机制（`api.rs` restart → `tray_proxy` 唤醒 tao → 优雅退出） | 升级重启可复用这条路径，在 server join（端口释放）之后 spawn 新进程 |
| 二进制体积敏感（`opt-level = "z"` + LTO + strip，托盘已 +400 KB） | 新增网络栈约 +0.8~1.2 MB（rustls 占大头），可接受；见 §8 |
| `config.toml` 三层合并 + 全 `Option<T>` | 新增 `auto_update_check` / `skip_version` 字段零成本 |
| token / 无 CORS 的安全模型 | 更新是**新增的 PC → GitHub 出站通道**，与现有手机 → PC 入站通道独立，需要自己的安全分析（§6） |

---

## 2. 生态调研

| 方案 | 机制 | 结论 |
|---|---|---|
| [`self_update`](https://github.com/jaemk/self_update)（jaemk） | 拉GitHub/GitLab Releases，下载「约定的压缩包」（`name-version-target.tar.gz`），解压替换二进制。Unix 上替换文件 + `exec` 保 PID；Windows 上 rename `.old` dance + spawn 新进程 | **不直接采用**。① 要求 per-target 压缩包资产命名，与我们「Windows/Linux 裸二进制 + macOS .app.zip」的资产布局不符；② **无签名校验**，只信 HTTPS；③ 完全不认识 .app bundle。它解决的「文件替换」问题用 `self-replace` 更干净 |
| [`self-replace`](https://github.com/mitsuhiko/self-replace)（mitsuhiko） | 只做一件事：安全地替换正在运行的自身可执行文件。Unix 上 rename 覆盖（运行中进程持有 inode 不受影响）；Windows 上处理了锁文件 rename dance 与残留清理 | **采用**（Windows/Linux/macOS 裸二进制形态）。ruff/uv 生态实战检验，质量可信 |
| `cargo-dist` + `axoupdater`（axodotdev） | cargo-dist 生成安装器（shell/PowerShell/homebrew tap），axoupdater 作为库或独立程序做更新 | **排除**。Axo 公司 2025 年停止运营，cargo-dist（改名 `dist`）进入维护/停更状态，0.28.1（2025-07）只是修 GitHub Actions 的收尾版本。现在把发布流水线迁上去是接盘一个死项目 |
| Tauri v2 updater 插件 | `latest.json` manifest + minisign（Ed25519）签名，下载验签后替换 | **机制借鉴，代码不可用**（绑定 Tauri 运行时）。它的「manifest 带签名、公钥编进 app」模型正是我们要抄的 |
| 包管理器分发（scoop / homebrew cask / AUR / choco） | 更新交给 OS 包管理器，app 只检查 + 提示 | **不作为主路径**。qrctrl 目标用户是「双击 exe / 双击 .app」人群，装 scoop/brew 的比例低；且要维护三套 manifest。作为补充渠道可以以后加 |
| 极简方案：托盘菜单「检查更新」→ 打开 GitHub Releases 页 | 纯跳转 | **保底备选**。零代码零风险，如果最终不想引入 +1 MB 网络栈，退回这条路线（§10） |

---

## 3. 推荐架构

### 3.1 更新清单（manifest）

在 Release 里额外上传一个 `update-manifest.json` 资产（不解析 release body——markdown 是给人看的，结构化数据走 JSON）：

```json
{
  "version": "0.10.0",
  "date": "2026-09-01T00:00:00Z",
  "notes_url": "https://github.com/wuyan19/qrctrl/releases/tag/v0.10.0",
  "platforms": {
    "windows-x86_64": {
      "url": "https://github.com/wuyan19/qrctrl/releases/download/v0.10.0/qrctrl-x86_64-windows.exe",
      "size": 5242880,
      "sha256": "…",
      "signature": "…minisign 签名（对文件字节）…"
    },
    "macos-aarch64-app":  { "url": "…/qrctrl-aarch64-macos.app.zip", "size": 0, "sha256": "…", "signature": "…" },
    "macos-x86_64-app":   { "url": "…", "sha256": "…", "signature": "…" },
    "linux-x86_64":       { "url": "…", "sha256": "…", "signature": "…" }
  }
}
```

- **manifest 本身也整体签一次名**（顶层 `manifest_signature` 字段），防攻击者只篡改单个平台的 sha256/signature 对。公钥列表 `include_str!` 编进 binary，支持多把（密钥轮换期新旧并存）。
- P0 阶段可以先只有 `sha256`（完整性 + 防 CDN 损坏），P1 加签名字段。

### 3.2 客户端流程

```
托盘「检查更新…」/ 启动后台检查（可配置，默认只查不装）
  → GET https://api.github.com/repos/wuyan19/qrctrl/releases/latest（10s 超时，必须带 User-Agent）
  → 从 assets 里找 update-manifest.json（拿 browser_download_url）
  → 下载 manifest → （P1：验签 manifest）
  → semver 比较 tag_name（strip 'v'） vs env!("CARGO_PKG_VERSION")
  → 用户确认（配置页区块点「立即更新」）
  → 下载对应平台资产到临时目录（流式写盘，按 manifest.size 预检 + 上限硬编码如 100 MB）
  → sha256 校验 → （P1：minisign 验签文件字节）
  → 平台替换（§4）→ 触发退出路径（复用现有 restart 的 shutdown_notify 机制）
  → server 线程 join（端口释放）+ 托盘 loop 退出后，spawn 新进程（带 --post-update 参数，§4.1）
```

- 检查失败**静默降级**（`eprintln!` 记日志），绝不影响主功能——这是后台 app，更新通道挂了不能弹错误到用户脸上。
- 开发模式检测：`current_exe()` 不在 `target/debug`、且（macOS 上）位于 `.app` bundle 内，才启用菜单项；`cargo run` 时禁用，防止开发者把自己 target 目录里的二进制换掉。
- GitHub API 细节：未认证限速 60 次/小时/源 IP（手动 + 每 24h 自动检查完全够用）；`browser_download_url` 会 302 到 `objects.githubusercontent.com`，HTTP 客户端要跟随重定向。

### 3.3 版本比较

`semver::Version::parse(tag.trim_start_matches('v'))`，大于当前版本才算有更新。`/releases/latest` 天然不返回 prerelease 和 draft，客户端无需额外处理。

---

## 4. 平台替换细节

### 4.1 Windows

- **文件替换**：运行中的 exe 无法删除/覆盖，但可以 rename。`self-replace` 内部处理（临时副本 + rename 交换 + 残留清理），无需手写 `.old` dance。
- **资产形态**：裸 `.exe` 直接下载即用，不用解压。
- **重启时序**（本项目特有难点）：
  1. 替换完成后不立刻 spawn——先走现有优雅退出（`Notify` → axum graceful shutdown → server 线程 join），此时 8080 端口已释放；
  2. 托盘 event loop 退出的最后一步 spawn 新进程；
  3. 保险起见给新进程传隐藏参数 `--post-update <old_pid>`：启动时先等旧进程退出（`OpenProcess` + `WaitForSingleObject`，带超时）再继续。没有这一步，万一端口还没释放，`probe_port` 会静默滑到 8081，QR 码指向就变了。
  4. GUI subsystem 无 console 继承问题；spawn 用 `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` creation flags（`windows-sys` 已在依赖里）。
- **权限边界**：exe 放在 `Program Files` 或其它用户不可写目录时替换会失败——捕获错误后提示「请手动下载更新」，不 retry 不硬来。

### 4.2 macOS

- **只支持 .app 安装形态**：`current_exe()` 向上找 `.app` bundle 祖先；找不到（裸二进制 / cargo run）则禁用更新。
- **整包替换而非替换内部二进制**：ad-hoc 签名覆盖 bundle 内 Mach-O + 资源，只换 `Contents/MacOS/qrctrl` 会让签名失效（启动被拦）；Info.plist 版本号也会 stale。所以单位是整个 `qrctrl.app/`：
  1. 下载 `.app.zip`（`ditto -c -k --keepParent` 格式，zip 根就是 `qrctrl.app/`）；
  2. 验签/校验 zip 字节 → `zip` crate 解压到同级临时目录（恢复 unix 权限位，`Contents/MacOS/qrctrl` 置 0755；bundle 内无 symlink，zip crate 足够，不用 shell out `ditto -x`）；
  3. `fs::rename` 旧 `.app` → 临时位置，`fs::rename` 新 `.app` → 原路径（同卷 rename，接近原子；失败则 rename 回滚）；
  4. 退出路径同 Windows：server join 后用 `open -n <bundle>` 拉起新实例（走 LaunchServices，LSUIElement 环境正确初始化；`-n` 强制新实例）。
- **运行中移动 .app 是安全的**：与 Unix 替换二进制同理，进程持有的是 inode。
- **Gatekeeper 红利**：app 自己用 ureq 下载的文件不带 `com.apple.quarantine`，首次运行不会被 Gatekeeper 拦——比用户从浏览器下载 zip 再解压的现行体验反而更顺。若未来上 Developer ID + notarization，替换逻辑不变，manifest 签名密钥体系换掉即可。
- **只读/受限位置**（从 DMG 里直接跑、无写权限）：检测写权限失败 → 引导手动更新。

### 4.3 Linux

- 裸二进制：下载到同目录临时文件 + `chmod 755` + rename 覆盖（运行中进程持 inode，不受影响），`self-replace` 一步到位。
- `cargo install --path .` 安装的场景：替换 `~/.cargo/bin/qrctrl` 后 cargo 的 registry 记录仍是旧版本（`cargo install` 会拒绝重装「同版本」）——文档里注明这类用户用 `cargo install --force` 或走更新器皆可，属已知小坑不是阻塞项。

---

## 5. 发布侧（CI）改动

`release.yml` 每个 matrix job 构建完成后新增产出元数据步骤，最后一个 job（或单独 job）聚合生成 manifest：

1. 各平台 job 输出 `asset → sha256`（`sha256sum`）到 workflow artifact；
2. 聚合 job：拼 `update-manifest.json`；
3. P1 起加签名：ubuntu 上跑仓库内的小 helper（`examples/sign_update.rs`，`minisign` crate，私钥和口令从 GitHub Secrets 注入），私钥**永不入库永不进 artifact**；公钥提交在 `assets/update-pubkey.txt`，`include_str!` 进代码；
4. manifest 作为普通资产随 draft release 上传。

**流程纪律**：现有 `draft: true` 保留（发布前人肉过目），但必须 publish 后更新才可见。可以考虑加一个 publish 后的 smoke job：匿名调 `/releases/latest` 确认 manifest 在 assets 里，防「忘了 publish / manifest 名字打错」这类静默失败。

---

## 6. 安全分析

**威胁模型**：qrctrl 持有输入注入 + 剪贴板读写 + 文件写入权限，等于常驻 RCE。更新通道被劫持 = 攻击者获得**持久化** RCE（每次开机自动以新「版本」运行恶意代码）。所以更新通道的安全等级应该对齐主通道（token 鉴权）而不是当普通下载对待。

| 防线 | 防什么 | 成本 |
|---|---|---|
| HTTPS（GitHub API + 资产域） | 局域网嗅探 / 明文篡改 | 免费（ureq 默认 rustls） |
| sha256（manifest 内） | 下载损坏 / 截断 / CDN 错误 | 一行代码 |
| minisign Ed25519 验签（manifest + 文件） | **GitHub 账号被盗、repo 被攻破、恶意 fork PR 合入**——攻击者拿不到私钥就推不了能通过验签的更新 | P1：CI 一步 + 公钥嵌入 + 验签 ~50 行 |
| 更新源 URL 硬编码，**不进 config.toml** | 攻击者诱导用户把更新指向自己服务器（一旦可配置，签名体系形同虚设的社工入口） | 零成本，纯纪律 |
| 多公钥列表（`Vec<&str>`） | 密钥轮换：新版信任新旧两把，过渡一个版本后删旧 | 略增复杂度 |

**不做**（明确划出去的）：自动静默安装（下载+替换必须用户点击确认）、自动检查默认值建议 `true` 但**只检查不下载**、下载上限硬编码（防 manifest 被篡改后 size 巨大撑爆磁盘）。

**失败回滚**：替换阶段的失败（rename 失败、写盘失败）就地回滚旧文件；「新版本起来就 crash」的失败 P0 不处理（用户手动重下），P2 可加健康标记（启动时写 pending 文件、正常 serve 后清除，下次启动发现 pending 未清 → 自动回滚备份的旧文件）。

---

## 7. UX 集成

- **托盘菜单**加「检查更新…」项：spawn 后台线程跑检查（绝不阻塞 tao event loop）；结果落到配置页的「关于 / 更新」区块（`config.html` 新增 section：当前版本、最新版本、更新日志链接、「立即更新」按钮 + 进度条）。不在托盘弹原生通知——tray-icon 无通知 API，引 rfd/notify-rust 又是两个依赖且 macOS 各有限制。
- 更新进度走 `/api/update/status` 轮询（或复用现有 HTTP 风格），下载完成由用户点「重启并安装」（与现有「重启确认弹窗」交互一致）。
- WS `server_info` 消息加 `version` 字段；P2 再加 `update_available` 推送，手机端状态栏显示「PC 端有新版本」横幅（点击只是打开 PC 配置页提示，手机端不参与安装）。
- config.toml 新增：`auto_update_check: Option<bool>`（默认 true，仅检查）、`skip_version: Option<String>`（跳过某版本不再提示）。

---

## 8. 依赖与体积成本

| crate | 用途 | 体积影响（release，估） |
|---|---|---|
| `ureq`（默认 rustls） | HTTP GET + 下载 | rustls 是大头，~600-800 KB |
| `semver` | 版本比较 | ~20 KB |
| `self-replace` | 替换自身 | ~10 KB |
| `rust-minisign-verify` | Ed25519 验签 | ~50 KB |
| `zip`（仅 macOS 路径用到，但会进统一 binary） | 解 .app.zip | ~150-250 KB |

合计约 **+0.8~1.2 MB**（`opt-level = "z"` + LTO 会再压一些）。对比：托盘功能已经 +400 KB，Windows release 现体量下可接受。若嫌重，唯一省体积的替代是 shell out 系统 `curl`——否决：Windows 老版本不带 curl、错误处理与进度反馈丑、引 shell 注入面。

## 9. 分阶段落地

| 阶段 | 内容 | 规模 |
|---|---|---|
| **P0** | manifest（仅 sha256）+ 检查/下载/校验/替换/重启全链路 + 托盘菜单 + 配置页区块 + `--post-update` 端口竞争防护 + CI manifest 生成 | ~2-3 天 |
| **P1** | minisign 全链路签名验签 + CI 签名步骤 + 密钥轮换支持 | ~1 天 |
| **P2** | 自动检查（24h）+ skip_version + 手机端提示 + 健康标记回滚 | ~1-2 天 |

新模块建议：`update.rs`（检查/下载/校验，纯逻辑可单测 mock HTTP）、`update_apply.rs` 或并入（平台替换，cfg 分支）；CI helper `examples/sign_update.rs`。`state.rs` 的 CoreState **不要**持有更新状态——更新是进程级动作不是业务核心，放 AppState 层即可。

## 10. 备选方案对照（何时退回）

- **A. 纯跳转**：托盘「检查更新」= `open_url_in_browser(releases 页)`。零依赖零风险，如果做完 P0 发现 +1 MB 网络栈不可接受、或维护精力不足，退到这里。代价：用户手动下载解压，macOS 还要过 Gatekeeper 右键打开。
- **B. 包管理器**：scoop manifest / homebrew cask / AUR 各发一份。零自研更新代码，但受众覆盖差（目标用户不装这些），且三套 manifest 的维护负担比一个 updater 还高。仅当社区有人主动要时再加。
- **C. cargo-dist/axoupdater**：已停维护，排除（§2）。

## 11. 参考链接

- [jaemk/self_update](https://github.com/jaemk/self_update) · [crates.io/self_update](https://crates.io/crates/self_update)
- [mitsuhiko/self-replace](https://github.com/mitsuhiko/self-replace)
- [axodotdev/axoupdater](https://github.com/axodotdev/axoupdater) · [cargo-dist CHANGELOG](https://github.com/axodotdev/cargo-dist/blob/main/CHANGELOG.md)（0.28.1 / 2025-07，维护状态）
- [Tauri v2 Updater 插件文档](https://v2.tauri.app/plugin/updater/)（minisign 签名模型来源）
- [rust-minisign-verify](https://crates.io/crates/rust-minisign-verify)
- [GitHub REST API: Get latest release](https://docs.github.com/en/rest/releases/releases#get-the-latest-release)（draft/prerelease 排除语义、User-Agent 要求、60/h 未认证限速）
