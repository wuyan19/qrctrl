const initialToken = new URLSearchParams(location.search).get('t') || '';
let currentToken = initialToken;
let modalCurrentPath = null;
// config.toml 里的 skip_version（loadConfig 回填，saveConfig 原样带回——
// 它由「跳过此版本」按钮管理，不进表单，但保存时必须带上防擦除）。
let savedSkipVersion = null;
// 当前主题偏好（"dark"/"light"/"system"），用于系统主题变化时判断是否需要跟随。
let currentThemePref = document.documentElement.getAttribute('data-theme') || 'system';

function showToast(msg, ms = 1800) {
  const el = document.getElementById('toast');
  el.textContent = msg;
  el.classList.add('show');
  clearTimeout(showToast._t);
  showToast._t = setTimeout(() => el.classList.remove('show'), ms);
}

function apiUrl(path, params = {}) {
  const u = new URL(path, location.origin);
  params.t = currentToken;
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== null) u.searchParams.set(k, v);
  }
  return u.toString();
}

async function fetchJson(url, opts) {
  const res = await fetch(url, opts);
  if (res.status === 401) {
    showToast('Token 失效，请从托盘菜单重新打开配置页');
    throw new Error('unauthorized');
  }
  return res;
}

// 把字节转成 {value, unit}，方便回填表单
function bytesToUnit(bytes) {
  if (bytes >= 1024 * 1024 * 1024 && bytes % (1024 * 1024 * 1024) === 0) {
    return { value: bytes / (1024 * 1024 * 1024), unit: 'GB' };
  }
  if (bytes >= 1024 * 1024 && bytes % (1024 * 1024) === 0) {
    return { value: bytes / (1024 * 1024), unit: 'MB' };
  }
  if (bytes >= 1024 && bytes % 1024 === 0) {
    return { value: bytes / 1024, unit: 'KB' };
  }
  return { value: bytes, unit: 'B' };
}

function unitToBytes(value, unit) {
  const v = Number(value);
  if (!Number.isFinite(v) || v <= 0) return 0;
  switch (unit) {
    case 'KB': return Math.floor(v * 1024);
    case 'MB': return Math.floor(v * 1024 * 1024);
    case 'GB': return Math.floor(v * 1024 * 1024 * 1024);
    default:   return Math.floor(v);
  }
}

async function loadConfig() {
  try {
    const res = await fetchJson(apiUrl('/api/config'));
    const cfg = await res.json();
    document.getElementById('f-name').value = cfg.name || '';
    document.getElementById('f-addr').value = cfg.addr || '';
    document.getElementById('f-port').value = cfg.port || '';
    document.getElementById('f-save-dir').value = cfg.save_dir || '';
    const ms = bytesToUnit(cfg.max_size || 0);
    document.getElementById('f-max-size').value = ms.value;
    document.getElementById('f-max-unit').value = ms.unit;
    document.getElementById('f-token').value = cfg.token || '';
    // mouse_sensitivity：clamp 到 [0.1, 5.0]，缺失时默认 1.0
    const sens = Math.min(5.0, Math.max(0.1, Number(cfg.mouse_sensitivity) || 1.0));
    document.getElementById('f-mouse-sens').value = sens;
    document.getElementById('f-mouse-sens-val').textContent = sens.toFixed(1) + '×';
    // prefer_ip 选中（loadLocalIps 已填充选项）
    const prefSel = document.getElementById('f-prefer-ip');
    const pref = cfg.prefer_ip || '';
    if (!pref || [...prefSel.options].some(o => o.value === pref)) {
      prefSel.value = pref;
    }
    // skip_version 存内存：saveConfig 时原样带回，防止 POST /api/config 把文件里
    // 该字段擦成 None（该值由「跳过此版本」按钮管理，不在表单里直接编辑）。
    savedSkipVersion = cfg.skip_version || null;
    // 应用主题：cfg.theme 可能是 "dark"/"light"/"system"（或缺失 → system 跟随系统）。
    // 与首屏 inline script 一致——这里再次应用是冗余但幂等的，主要给后端被改后的情况兜底。
    applyTheme(cfg.theme || 'system');
  } catch (e) {
    console.error(e);
  }
}

// 主题应用：system 用 prefers-color-scheme 解析，其他直接用。
function applyTheme(theme) {
  currentThemePref = theme || 'system';
  let resolved = theme;
  if (theme === 'system' || !theme) {
    resolved = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  document.documentElement.setAttribute('data-theme', resolved);
}

// 系统主题变化时，仅当用户偏好为 system 时跟随。否则保持用户显式选择。
if (window.matchMedia) {
  window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => {
    if (currentThemePref === 'system') {
      applyTheme('system');
    }
  });
}

async function loadLocalIps(selectedPref) {
  try {
    const res = await fetchJson(apiUrl('/api/local_ips'));
    const ifs = await res.json();
    const sel = document.getElementById('f-prefer-ip');
    const cur = selectedPref || sel.value;
    sel.innerHTML = '<option value="">无偏好</option>';
    // 列出所有接口（不去重）：网卡名 + IP + CIDR，让用户一眼挑出想要的网卡。
    // 多个 IP 共享同一 prefer 前缀时它们指向同一个值（前缀过滤），但展示分开便于识别。
    for (const f of ifs) {
      const opt = document.createElement('option');
      opt.value = f.prefer;
      opt.textContent = `${f.name}  ·  ${f.ip}  (${f.cidr})`;
      sel.appendChild(opt);
    }
    if (cur && [...sel.options].some(o => o.value === cur)) {
      sel.value = cur;
    }
  } catch (e) {
    console.error(e);
  }
}

// ============= 端口可用性预检 =============
async function checkPort() {
  const portEl = document.getElementById('f-port');
  const hintEl = document.getElementById('port-hint');
  const addr = document.getElementById('f-addr').value.trim() || '0.0.0.0';
  const port = portEl.value.trim();
  if (!port) {
    portEl.classList.remove('invalid');
    hintEl.textContent = '留空 = 从 8080 起自动找一个可用端口。';
    hintEl.style.color = '';
    return;
  }
  try {
    const res = await fetchJson(apiUrl('/api/check_port', { addr, port }));
    const data = await res.json();
    if (data.free) {
      portEl.classList.remove('invalid');
      if (data.self) {
        hintEl.textContent = '· 当前 qrctrl 正在使用此端口';
        hintEl.style.color = 'var(--cyan)';
      } else {
        hintEl.textContent = '✓ 可用';
        hintEl.style.color = 'var(--amber)';
      }
    } else {
      portEl.classList.add('invalid');
      hintEl.textContent = '✗ 该端口已被占用或权限不足';
      hintEl.style.color = 'var(--red)';
    }
  } catch (e) { /* 忽略 */ }
}

// ============= 目录选择模态 =============
async function openDirPicker() {
  document.getElementById('dir-modal').classList.add('show');
  await navigateDir(null);
}

async function navigateDir(path) {
  try {
    const params = {};
    if (path) params.path = path;
    const res = await fetchJson(apiUrl('/api/list_dir', params));
    const data = await res.json();
    const isRoots = data.is_roots === true;

    // roots 视图下不更新 modalCurrentPath——「选择此目录」按钮也禁用，
    // 因为 roots 本身不是有效目录。保留上次的具体路径以便用户返回。
    if (!isRoots) {
      modalCurrentPath = data.current;
    }
    document.getElementById('modal-current').textContent = isRoots ? '此电脑' : data.current;
    // 选择按钮：roots 视图下禁用
    document.getElementById('modal-select').disabled = isRoots;

    // 面包屑（类 Finder 风格：用 › 分隔）
    const bcEl = document.getElementById('modal-breadcrumb');
    bcEl.innerHTML = '';

    // 分隔符工具：在两个元素之间插一个「 › 」
    const pushSep = () => {
      const s = document.createElement('span');
      s.className = 'sep';
      s.textContent = '›';
      bcEl.appendChild(s);
    };

    // 最前的「此电脑」入口（跨平台统一文案）
    const rootLink = document.createElement('a');
    rootLink.className = 'root-link';
    rootLink.textContent = '💻 此电脑';
    rootLink.onclick = (e) => { e.preventDefault(); navigateDir('roots'); };
    bcEl.appendChild(rootLink);

    if (!isRoots) {
      const isWin = data.current.includes('\\') || /^[A-Z]:/i.test(data.current);
      const sepChar = isWin ? '\\' : '/';
      const parts = data.current.split(/[\\/]/).filter(Boolean);

      // 逐段构造面包屑。Windows: parts[0] = "D:"，acc 起步 "D:\\"；
      // Unix: 第一个段是根下的第一级目录，acc 起步 "/<part>"。
      let acc = isWin ? (parts[0] + '\\') : '';
      for (let i = 0; i < parts.length; i++) {
        if (i > 0) {
          acc = acc.replace(/[\\/]$/, '') + sepChar + parts[i];
        } else if (!isWin) {
          acc = '/' + parts[i];
        }
        pushSep();
        const a = document.createElement('a');
        a.textContent = parts[i];
        const target = acc;
        a.onclick = (e) => { e.preventDefault(); navigateDir(target); };
        bcEl.appendChild(a);
      }
    }

    // 条目
    const bodyEl = document.getElementById('modal-entries');
    bodyEl.innerHTML = '';

    if (isRoots) {
      // 列盘符（Windows）或 `/`（Unix）。每个 entry 自带 path 字段。
      if (data.entries.length === 0) {
        const empty = document.createElement('div');
        empty.className = 'dir-entry empty';
        empty.textContent = '（未检测到可用盘符）';
        bodyEl.appendChild(empty);
      }
      for (const e of data.entries) {
        const el = document.createElement('div');
        el.className = 'dir-entry';
        el.innerHTML = '<span class="icon">💽</span> ' + e.name;
        el.onclick = () => navigateDir(e.path);
        bodyEl.appendChild(el);
      }
      return;
    }

    // 上级目录
    if (data.parent) {
      const el = document.createElement('div');
      el.className = 'dir-entry';
      el.innerHTML = '<span class="icon">↑</span> ..';
      el.onclick = () => navigateDir(data.parent);
      bodyEl.appendChild(el);
    }
    if (data.entries.length === 0 && !data.parent) {
      const empty = document.createElement('div');
      empty.className = 'dir-entry empty';
      empty.textContent = '（空）';
      bodyEl.appendChild(empty);
    }
    const isWin = data.current.includes('\\') || /^[A-Z]:/i.test(data.current);
    const sepChar = isWin ? '\\' : '/';
    for (const e of data.entries) {
      const el = document.createElement('div');
      el.className = 'dir-entry';
      el.innerHTML = '<span class="icon">📁</span> ' + e.name;
      el.onclick = () => navigateDir(data.current + sepChar + e.name);
      bodyEl.appendChild(el);
    }
  } catch (e) {
    showToast('读取目录失败');
  }
}

function closeDirPicker() {
  document.getElementById('dir-modal').classList.remove('show');
}

function selectDir() {
  if (modalCurrentPath) {
    document.getElementById('f-save-dir').value = modalCurrentPath;
  }
  closeDirPicker();
}

// ============= 收集表单 → POST =============
async function saveConfig() {
  const portRaw = document.getElementById('f-port').value.trim();
  const cfg = {
    name: document.getElementById('f-name').value.trim() || null,
    addr: document.getElementById('f-addr').value.trim() || null,
    port: portRaw ? Number(portRaw) : null,
    save_dir: document.getElementById('f-save-dir').value.trim() || null,
    max_size: unitToBytes(
      document.getElementById('f-max-size').value,
      document.getElementById('f-max-unit').value
    ),
    token: document.getElementById('f-token').value.trim() || null,
    prefer_ip: document.getElementById('f-prefer-ip').value || null,
    // 灵敏度已通过 /api/mouse_sensitivity live-apply 写过文件，这里只是带着避免
    // set_config_handler 的 save(&payload) 把 None 写回成缺失字段（会擦掉之前 live-apply 的值）
    mouse_sensitivity: Number(document.getElementById('f-mouse-sens').value),
    // skip_version 由「跳过此版本」按钮管理（/api/update/skip 已实时写文件），
    // 保存时必须带上，否则 POST /api/config 会把 config.toml 里已有的值擦成未设置。
    skip_version: savedSkipVersion,
  };

  const saveBtn = document.getElementById('save-btn');
  saveBtn.disabled = true;
  saveBtn.textContent = '保存中...';
  try {
    const res = await fetchJson(apiUrl('/api/config'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(cfg),
    });
    const data = await res.json();
    if (!data.ok) {
      showToast('保存失败：' + (data.error || '未知错误'), 3000);
      return;
    }
    // 所有字段都不 live-apply——只写文件，下次启动才生效。
    // 不再回 new_token：之前 token 改了会换前端内存里的 currentToken，
    // 但 state.token 不可变、后端鉴权仍用旧 token，导致后续 fetch 401。
    // 统一重启生效消除这个 bug。
    const banner = document.getElementById('banner');
    banner.innerHTML = '已保存。<strong>请点击「重启 qrctrl」让所有配置（包括 token）生效。</strong>重启前当前会话仍用旧配置工作。';
    banner.classList.add('show');
    showToast('已保存，请重启 qrctrl');
  } catch (e) {
    showToast('网络错误');
  } finally {
    saveBtn.disabled = false;
    saveBtn.textContent = '保存';
  }
}

// ============= 通用确认模态 =============
// 替代 window.confirm——原生弹窗风格不统一。
// 返回 Promise<boolean>：true = 确认，false = 取消（含 ESC、背景点击）。
function showConfirm({ title, message, confirmText = '确认', cancelText = '取消', danger = false }) {
  return new Promise((resolve) => {
    const bg = document.createElement('div');
    bg.className = 'modal-bg';

    const modal = document.createElement('div');
    modal.className = 'modal confirm-modal';

    const header = document.createElement('div');
    header.className = 'modal-header';
    header.textContent = title;

    const body = document.createElement('div');
    body.className = 'confirm-body';
    body.innerHTML = message;

    const footer = document.createElement('div');
    footer.className = 'modal-footer';

    const actions = document.createElement('div');
    actions.className = 'modal-actions';

    const cancelBtn = document.createElement('button');
    cancelBtn.type = 'button';
    cancelBtn.className = 'btn-small';
    cancelBtn.textContent = cancelText;

    const confirmBtn = document.createElement('button');
    confirmBtn.type = 'button';
    confirmBtn.className = danger ? 'btn-small btn-confirm' : 'btn-small';
    confirmBtn.textContent = confirmText;

    actions.appendChild(cancelBtn);
    actions.appendChild(confirmBtn);
    footer.appendChild(actions);
    modal.appendChild(header);
    modal.appendChild(body);
    modal.appendChild(footer);
    bg.appendChild(modal);
    document.body.appendChild(bg);
    bg.classList.add('show');

    let done = false;
    function close(value) {
      if (done) return;
      done = true;
      bg.classList.remove('show');
      document.removeEventListener('keydown', onKey);
      bg.remove();
      resolve(value);
    }
    function onKey(e) {
      if (e.key === 'Escape') close(false);
      else if (e.key === 'Enter') close(true);
    }
    cancelBtn.onclick = () => close(false);
    confirmBtn.onclick = () => close(true);
    bg.onclick = (e) => { if (e.target === bg) close(false); };
    document.addEventListener('keydown', onKey);
    cancelBtn.focus();
  });
}

// ============= 重启 qrctrl =============
// 流程：POST /api/restart → server 触发 tray 退出 + 自己 graceful shutdown →
// tray 的 RestartRequested 分支 spawn 新进程接管 → 新进程 bind 同端口。
// fetch 大概率会因为 server 关闭而失败，这是预期内的。
// 之后轮询 /api/config 直到拿到任何 HTTP 响应（200 或 401）就算服务器回来了：
// - 200：旧 token 仍有效（用户没改 token 或改了又改回来）→ 自动刷新页面
// - 401：服务器回来了但旧 token 失效（用户改了 token）→ 提示用托盘菜单重进
//
// doRestartAndWait 是「发起 + 等恢复」的公共部分，两个调用方：
// - restartQrctrl（配置页「重启 qrctrl」按钮，带确认弹窗）
// - 更新流程（新版本装好后 phase = restart_pending 自动触发，确认在点「安装」时已做过）
async function doRestartAndWait({ doingText, doneText }) {
  const restartBtn = document.getElementById('restart-btn');
  restartBtn.disabled = true;
  restartBtn.textContent = doingText;

  const banner = document.getElementById('banner');
  banner.innerHTML = `<strong>${doingText}</strong>页面将在几秒后自动恢复。`;
  banner.classList.add('show');

  // 发起重启请求（fetch 会因为 server 关闭而失败，预期内）
  try {
    await fetch(apiUrl('/api/restart'), { method: 'POST', cache: 'no-store' });
  } catch (e) {
    // 预期内：server 关闭导致 fetch 失败
  }

  // 给老进程退出 + 新进程 spawn + bind 一点时间，再开始轮询
  await new Promise(r => setTimeout(r, 1200));

  // 轮询上限 30 秒；只要拿到任何 HTTP 响应就说明服务器回来了
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(apiUrl('/api/config'), { cache: 'no-store' });
      if (res.status === 200) {
        banner.innerHTML = `<strong>${doneText}</strong>正在刷新页面...`;
        setTimeout(() => location.reload(), 500);
        return;
      }
      if (res.status === 401) {
        // 服务器回来了但 URL 里的 token 失效（用户改过 token）
        banner.innerHTML = `<strong>${doneText}</strong>Token 已变更，请通过托盘菜单「配置」或重新扫码进入配置页。`;
        restartBtn.textContent = '已重启';
        return;
      }
      // 其他状态码不应该出现，继续轮询
    } catch (e) {
      // 服务器还没起来，继续等
    }
    await new Promise(r => setTimeout(r, 1000));
  }

  banner.innerHTML = '<strong>重启超时。</strong>30 秒内未检测到 qrctrl 恢复，请检查托盘图标是否存在，或手动重新启动。';
  restartBtn.disabled = false;
  restartBtn.textContent = '重启 qrctrl';
}

async function restartQrctrl() {
  const ok = await showConfirm({
    title: '重启 qrctrl',
    message: '确定要重启 qrctrl 吗？<br>重启期间手机端会短暂断连，此页面也会暂时无响应。<br>若是修改了Network/Security配置，需重新扫码或修改访问地址。',
    confirmText: '重启',
    cancelText: '取消',
    danger: true,
  });
  if (!ok) return;
  await doRestartAndWait({ doingText: '正在重启 qrctrl...', doneText: '重启完成。' });
}

// ============= 在线更新 =============
// 后端状态机见 src/update.rs 的 UpdatePhase（HTTP 面：/api/update/*）。
// 本区块只在「进行态」（checking / downloading / installing）时轮询（1s），
// 静止态（idle / up_to_date / available / failed）拉一次即停。
// restart_pending 时自动触发 doRestartAndWait——「重启并安装」一键完成。
let updatePollTimer = null;
let updateRestarting = false;
// 本页面会话内是否观察到过下载/安装进行态。restart_pending 的自动重启只对
// 「本会话里点了安装」的用户生效（延续「重启并安装」的一次性同意）；页面
// （重）加载后纯首拉命中 restart_pending 的，显示手动按钮——否则用户只是
// 打开配置页改个配置，就会被无预告重启，断开手机端的连接。
let sawInstallInFlight = false;
// 最近一次渲染的 phase。install / skip 的按钮处理器需要 version，从这取，
// 避免每次渲染往 DOM/全局塞数据。
let lastUpdatePhase = null;
// 轮询连续失败计数：后端彻底不可达时停止空转（而不是 1.5s 一次永远重试）
let updatePollFailures = 0;

function fmtBytes(n) {
  if (n >= 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MB';
  if (n >= 1024) return (n / 1024).toFixed(0) + ' KB';
  return n + ' B';
}

function setUpdateNote(text, cls) {
  const note = document.getElementById('update-note');
  note.textContent = text || '';
  note.className = 'update-note' + (cls ? ' ' + cls : '');
}

function showUpdateButtons({ check = false, checkText = '检查更新', install = false, skip = false, skipText = '跳过此版本', restart = false } = {}) {
  const checkBtn = document.getElementById('update-check-btn');
  checkBtn.hidden = !check;
  checkBtn.textContent = checkText;
  checkBtn.disabled = false;
  document.getElementById('update-install-btn').hidden = !install;
  const skipBtn = document.getElementById('update-skip-btn');
  skipBtn.hidden = !skip;
  skipBtn.textContent = skipText;
  document.getElementById('update-restart-btn').hidden = !restart;
}

function renderUpdateStatus(data) {
  const phase = data.phase || { state: 'idle' };
  lastUpdatePhase = phase;
  const progress = document.getElementById('update-progress');
  document.getElementById('update-notes-link').hidden = true;

  const busy = ['checking', 'downloading', 'installing'].includes(phase.state);
  switch (phase.state) {
    case 'idle':
      setUpdateNote('');
      showUpdateButtons({ check: true });
      progress.hidden = true;
      break;
    case 'checking':
      setUpdateNote('正在检查...');
      showUpdateButtons({});
      progress.hidden = true;
      break;
    case 'up_to_date':
      setUpdateNote('已是最新', 'ok');
      showUpdateButtons({ check: true, checkText: '重新检查' });
      progress.hidden = true;
      break;
    case 'available': {
      const skipped = phase.skipped === true;
      if (skipped) {
        setUpdateNote(`v${phase.version}（已跳过）`, 'warn');
        showUpdateButtons({ check: true, checkText: '重新检查', skip: true, skipText: '取消跳过' });
      } else {
        setUpdateNote(`v${phase.version} · ${fmtBytes(phase.size || 0)}`, 'warn');
        showUpdateButtons({ check: true, checkText: '重新检查', install: true, skip: true });
      }
      if (phase.notes_url) {
        const link = document.getElementById('update-notes-link');
        link.href = phase.notes_url;
        link.hidden = false;
      }
      progress.hidden = true;
      break;
    }
    case 'downloading': {
      sawInstallInFlight = true;
      setUpdateNote(`正在下载 v${phase.version}...`);
      progress.hidden = false;
      const total = phase.total || 0;
      const bytes = phase.bytes || 0;
      const pct = total > 0 ? Math.min(100, Math.round(bytes / total * 100)) : 0;
      document.getElementById('update-progress-fill').style.width = pct + '%';
      document.getElementById('update-progress-text').textContent = `${fmtBytes(bytes)} / ${fmtBytes(total)}`;
      showUpdateButtons({});
      break;
    }
    case 'installing':
      sawInstallInFlight = true;
      setUpdateNote('正在安装...');
      progress.hidden = true;
      showUpdateButtons({});
      break;
    case 'restart_pending':
      progress.hidden = true;
      if (sawInstallInFlight) {
        // 本会话内从安装流程走过来：延续「重启并安装」的确认，自动完成收尾
        setUpdateNote('重启中...', 'ok');
        showUpdateButtons({});
        triggerUpdateRestart();
      } else {
        // 页面（重）加载后首拉命中：安装早已完成，只差重启——交给用户决定
        setUpdateNote('新版本已就位', 'ok');
        showUpdateButtons({ restart: true });
      }
      return 'stop';
    case 'failed':
      // 不透出后端错误详情（对用户是噪音）——固定文案，详情进后端日志（update.rs 的 tracing）。
      setUpdateNote('获取新版本失败', 'error');
      showUpdateButtons({ check: true, checkText: '重试' });
      progress.hidden = true;
      break;
    default:
      setUpdateNote('获取新版本失败', 'error');
      showUpdateButtons({ check: true, checkText: '重试' });
      progress.hidden = true;
  }
  return busy ? 'poll' : 'stop';
}

// restart_pending 只自动触发一次：页面在重启过程中可能重复拉到该状态。
function triggerUpdateRestart() {
  if (updateRestarting) return;
  updateRestarting = true;
  document.getElementById('update-restart-btn').hidden = true;
  doRestartAndWait({ doingText: '正在重启到新版本...', doneText: '更新完成。' });
}

async function pollUpdateStatus() {
  try {
    const res = await fetchJson(apiUrl('/api/update/status'));
    const data = await res.json();
    updatePollFailures = 0;
    const action = renderUpdateStatus(data);
    clearTimeout(updatePollTimer);
    if (action === 'poll') {
      updatePollTimer = setTimeout(pollUpdateStatus, 1000);
    }
  } catch (e) {
    // 网络抖动：重试；连续 5 次不可达则放弃轮询（后端彻底不在了——
    // 比如用户已手动重启进程，页面刷新后自然恢复）。
    updatePollFailures += 1;
    clearTimeout(updatePollTimer);
    if (updatePollFailures < 5) {
      updatePollTimer = setTimeout(pollUpdateStatus, 1500);
    } else {
      setUpdateNote('状态获取失败', 'error');
    }
  }
}

async function checkForUpdate() {
  const checkBtn = document.getElementById('update-check-btn');
  checkBtn.disabled = true;
  checkBtn.textContent = '检查中...';
  try {
    await fetchJson(apiUrl('/api/update/check'), { method: 'POST' });
  } catch (e) {
    showToast('发起检查失败');
  }
  // 无论成败都以 status 渲染（失败态由后端 phase = failed 表达）
  pollUpdateStatus();
}

async function installUpdate(version) {
  const ok = await showConfirm({
    title: '安装更新',
    message: `将下载 v${version} 并替换当前程序，完成后<strong>自动重启 qrctrl</strong>。<br>重启期间手机端会短暂断连，此页面会暂时无响应。`,
    confirmText: '重启并安装',
    cancelText: '取消',
  });
  if (!ok) return;
  try {
    const res = await fetchJson(apiUrl('/api/update/install'), { method: 'POST' });
    const data = await res.json();
    if (!data.ok) {
      showToast('安装失败：' + (data.error || '未知错误'), 3000);
      return;
    }
    pollUpdateStatus();
  } catch (e) {
    showToast('网络错误');
  }
}

async function skipUpdate(version, skip) {
  try {
    const res = await fetchJson(apiUrl('/api/update/skip'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ version, skip }),
    });
    // 同步内存副本：下次 saveConfig POST 时带上正确值，避免把刚写入 config.toml
    // 的 skip_version 又擦回旧值。「取消跳过」只在 skipped=true 时可点，置 null 正确。
    if (res.ok) savedSkipVersion = skip ? version : null;
    pollUpdateStatus();
  } catch (e) {
    showToast('操作失败');
  }
}

// ============= 初始化 =============
function bindEvents() {
  document.getElementById('browse-btn').onclick = openDirPicker;
  document.getElementById('modal-cancel').onclick = closeDirPicker;
  document.getElementById('modal-select').onclick = selectDir;
  document.getElementById('save-btn').onclick = saveConfig;
  document.getElementById('restart-btn').onclick = restartQrctrl;
  document.getElementById('f-port').addEventListener('blur', checkPort);
  document.getElementById('f-addr').addEventListener('blur', checkPort);

  // 更新区块
  document.getElementById('update-check-btn').onclick = checkForUpdate;
  document.getElementById('update-install-btn').onclick = () => {
    if (lastUpdatePhase && lastUpdatePhase.version) installUpdate(lastUpdatePhase.version);
  };
  document.getElementById('update-restart-btn').onclick = triggerUpdateRestart;
  document.getElementById('update-skip-btn').onclick = () => {
    if (!lastUpdatePhase || !lastUpdatePhase.version) return;
    // 按钮文案即当前语义：「取消跳过」= 当前处于跳过态，发 skip=false
    const isSkipped = lastUpdatePhase.skipped === true;
    skipUpdate(lastUpdatePhase.version, !isSkipped);
  };

  // 灵敏度滑块：input 时实时更新数值显示，change（松开）时 POST 后端 live-apply
  const sensEl = document.getElementById('f-mouse-sens');
  const sensVal = document.getElementById('f-mouse-sens-val');
  sensEl.addEventListener('input', () => {
    sensVal.textContent = Number(sensEl.value).toFixed(1) + '×';
  });
  sensEl.addEventListener('change', setMouseSensitivity);

  document.getElementById('token-toggle').onclick = () => {
    const el = document.getElementById('f-token');
    el.type = el.type === 'password' ? 'text' : 'password';
  };
  // 默认隐藏 token
  document.getElementById('f-token').type = 'password';
}

// 触控板灵敏度 live-apply：直接 POST /api/mouse_sensitivity，后端立即改 state + 写文件。
// 不走 saveConfig 路径——那条路径所有字段都要重启生效，灵敏度不需要。
async function setMouseSensitivity() {
  const v = Number(document.getElementById('f-mouse-sens').value);
  try {
    const res = await fetchJson(apiUrl('/api/mouse_sensitivity'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mouse_sensitivity: v }),
    });
    const r = await res.json();
    if (!r.ok) {
      showToast('保存失败：' + (r.error || '未知错误'));
      // 失败时回滚到后端当前值
      await loadConfig();
    }
  } catch (e) {
    showToast('网络错误：' + e.message);
  }
}

window.addEventListener('DOMContentLoaded', async () => {
  // 返回控制台链接（用初始 token，保存后如改了 token 会被 saveConfig 更新）
  const backUrl = new URL(location.pathname.replace('/config', '/'), location.origin);
  backUrl.searchParams.set('t', initialToken);
  document.getElementById('back-link').href = backUrl.toString();

  bindEvents();

  // 串行：先拉候选 IP 填充下拉框，再拉当前配置回填（确保 prefer_ip 选中状态不丢）
  await loadLocalIps();
  await loadConfig();

  // 更新区块：进页面拉一次当前状态，渲染 idle / 历史结果
  pollUpdateStatus();
});
