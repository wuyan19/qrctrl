const token = new URLSearchParams(location.search).get('t') || '';
const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
const wsUrl = `${proto}//${location.host}/ws?t=${encodeURIComponent(token)}`;
const MAX_IMG_B64 = 10_000_000;

const statusEl = document.getElementById('status');
const statusText = document.getElementById('status-text');
const textEl = document.getElementById('text');
const sendBtn = document.getElementById('send');
const clearBtn = document.getElementById('clear');
const pullTextBtn = document.getElementById('btn-pull-text');
const pullImageBtn = document.getElementById('btn-pull-image');
const pickImageBtn = document.getElementById('btn-pick-image');
const sendFileBtn = document.getElementById('btn-send-file');
const pullFileBtn = document.getElementById('btn-pull-file');
const filePick = document.getElementById('file-pick');
const fileSend = document.getElementById('file-send');
const modal = document.getElementById('img-modal');
const modalImg = document.getElementById('modal-img');
const modalClose = document.getElementById('modal-close');
const transferModal = document.getElementById('transfer-modal');
const transferTitle = document.getElementById('transfer-title');
const transferProgress = document.getElementById('transfer-progress');
const transferStats = document.getElementById('transfer-stats');
const transferCancel = document.getElementById('transfer-cancel');
const fileListModal = document.getElementById('file-list-modal');
const fileListItems = document.getElementById('file-list-items');
const fileListClose = document.getElementById('file-list-close');
const fileListDownloadAll = document.getElementById('file-list-download-all');
const fileListDownload = document.getElementById('file-list-download');
// picker 当前显示的文件列表缓存 + 选中索引集合
let fileListCurrent = [];
let fileListSelected = new Set();
const toolPanel = document.getElementById('tool-panel');
const btnTools = document.getElementById('btn-tools');
const btnEnter = document.getElementById('btn-enter');
const btnCopy = document.getElementById('btn-copy');
const btnPaste = document.getElementById('btn-paste');
const toastEl = document.getElementById('toast');
const autoSendChk = document.getElementById('auto-send');
const helpBtn = document.getElementById('btn-help');
const helpModal = document.getElementById('help-modal');
const helpClose = document.getElementById('help-close');
const btnTouchpad = document.getElementById('btn-touchpad');
const touchpadModal = document.getElementById('touchpad-modal');
const touchpadClose = document.getElementById('touchpad-close');
const touchpadStatus = document.getElementById('touchpad-status');
const touchpadStatusText = document.getElementById('touchpad-status-text');
const touchArea = document.getElementById('touch-area');
const btnScrollUp = document.getElementById('btn-scroll-up');
const btnScrollDown = document.getElementById('btn-scroll-down');
const btnMouseLeft = document.getElementById('btn-mouse-left');
const btnMouseRight = document.getElementById('btn-mouse-right');
const btnTheme = document.getElementById('btn-theme');
const themeIcon = btnTheme ? btnTheme.querySelector('.theme-icon') : null;

// 主题偏好：'dark' / 'light' / 'system'。从 <html data-theme> 拿首屏值（服务端注入 +
// inline script 已 resolve 过）。后续 ws server_info 推过来时会覆盖。
// resolvedTheme 是 'dark' 或 'light'（system 已解析后的实际值），用于切换按钮决定下一态。
let currentTheme = document.documentElement.getAttribute('data-theme') || 'dark';

function resolvedThemeNow() {
  // data-theme 已经是 dark/light（inline script 已 resolve），直接读
  return document.documentElement.getAttribute('data-theme') || 'dark';
}

function applyThemeToDom(theme) {
  // theme: 'dark' / 'light' / 'system'。system → 用 prefers-color-scheme 解析。
  currentTheme = theme;
  let resolved = theme;
  if (theme === 'system' || !theme) {
    resolved = window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  document.documentElement.setAttribute('data-theme', resolved);
  updateThemeIcon(resolved);
}

function updateThemeIcon(resolved) {
  if (!themeIcon) return;
  // 当前是 dark 时显示☀（点击切到 light）；当前是 light 时显示☾（点击切到 dark）。
  // 用 ASCII 字符避免 emoji 渲染差异（不同平台字体宽度不一，按钮内会跳动）。
  themeIcon.textContent = resolved === 'dark' ? '☀' : '☾';
}

function toggleTheme() {
  const cur = resolvedThemeNow();
  const next = cur === 'dark' ? 'light' : 'dark';
  applyThemeToDom(next);
  // 持久化（live apply + 写 config.toml）。失败不回滚 UI——用户视觉偏好优先，
  // 文件写失败时下次启动会回到旧 theme，可接受。
  fetch(`${location.origin}/api/theme?t=${encodeURIComponent(token)}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ theme: next }),
  }).catch(() => {});
}

// 系统主题变化时，若当前偏好是 system，实时跟随（用户没显式选过 dark/light）
if (window.matchMedia) {
  window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => {
    if (currentTheme === 'system') {
      applyThemeToDom('system');
    }
  });
}

let ws = null;
let reconnectDelay = 1000;
let toastTimer = null;
let autoSendTimer = null;
let isComposing = false;
let lastSentType = null;
let deviceName = 'PC';
let currentUploadXHR = null;
// 多文件上传队列:用户选多个文件全部入队,串行传(收到 upload_ready 才发下一个)。
// 状态语义:
// - uploadQueue:待传文件(已入队但还没收到 upload_ready)
// - uploadCurrent:正在传的 File(对应 in-flight XHR)
// - uploadBytesDone:本批次累计已完成字节(已传完的文件 size 累加)
// - uploadTotalBytes / uploadTotalCount:本批次总字节 / 总文件数,用于聚合进度
// - uploadSavedNames:本批次每个文件落盘后的实际文件名（重名时 server 会加 UUID 后缀，
//   不等于上传时的 name）。批结束后一次性 set_clipboard_files 推剪贴板。
// 当队列空 + 没有当前 XHR 时表示批次结束,状态归零。
let uploadQueue = [];
let uploadCurrent = null;
let uploadBytesDone = 0;
let uploadTotalBytes = 0;
let uploadTotalCount = 0;
let uploadSavedNames = [];

function setStatus(s, connected) {
  statusText.textContent = s;
  statusEl.className = 'status ' + (connected ? 'connected' : 'disconnected');
  if (touchpadModal && touchpadModal.classList.contains('show')) {
    syncTouchpadStatus();
  }
}

function toast(msg) {
  toastEl.textContent = msg;
  toastEl.classList.add('show');
  if (toastTimer) clearTimeout(toastTimer);
  toastTimer = setTimeout(() => toastEl.classList.remove('show'), 2000);
}

function send(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(obj));
    lastSentType = obj.type;
    return true;
  }
  toast('未连接');
  return false;
}

function connect() {
  setStatus('连接中...', false);
  ws = new WebSocket(wsUrl);
  ws.onopen = () => {
    // 不立即 setStatus——等 server_info 推过来再显示设备名，避免「PC 已连接」闪一下
    sendBtn.disabled = false;
    reconnectDelay = 1000;
  };
  ws.onclose = () => {
    setStatus(`${deviceName} 已断开 · ${Math.round(reconnectDelay / 1000)}s 后重连`, false);
    sendBtn.disabled = true;
    setTimeout(connect, reconnectDelay);
    reconnectDelay = Math.min(reconnectDelay * 2, 10000);
  };
  ws.onerror = () => {
    setStatus(`${deviceName} 连接错误`, false);
  };
  ws.onmessage = (ev) => {
    let m;
    try { m = JSON.parse(ev.data); } catch { return; }
    switch (m.type) {
      case 'server_info':
        deviceName = m.name || 'PC';
        if (m.version) {
          document.getElementById('help-version').textContent = 'v' + m.version;
        }
        // 服务端推送当前主题偏好，覆盖首屏 inline script 的解析结果。
        // 用户在 PC 端切换 theme 后这里会立即同步（如果当前 ws 还连着）。
        if (m.theme) {
          applyThemeToDom(m.theme);
        }
        setStatus(`${deviceName} 已连接`, true);
        break;
      case 'ok':
        // 文本注入不弹 toast（用户能直接看到焦点窗口出字，频繁 toast 反而烦）
        if (lastSentType === 'set_clipboard_image') {
          toast(`已写入 ${deviceName} 剪贴板`);
        }
        break;
      case 'clipboard_text':  onPullText(m.content); break;
      case 'clipboard_image': onPullImage(m.data, m.mime); break;
      case 'upload_ready':    onUploadReady(m.url); break;
      case 'file_list':       onFileList(m.files); break;
      case 'empty':           toast(`${deviceName} 剪贴板为空`); break;
      case 'error':           toast('失败：' + (m.code || 'unknown')); break;
    }
  };
}
connect();

// 发送文本（统一 JSON 协议）
function sendText() {
  cancelAutoSend();
  const text = textEl.value;
  if (!text.trim()) return;
  if (send({ type: 'text', value: text })) {
    textEl.value = '';
  }
}

// 自动发送：输入停顿 600ms 后发送，IME 选词期间不触发（避免发送半截拼音）
function scheduleAutoSend() {
  if (!autoSendChk.checked) return;
  if (isComposing) return;
  if (autoSendTimer) clearTimeout(autoSendTimer);
  autoSendTimer = setTimeout(() => {
    autoSendTimer = null;
    sendText();
  }, 600);
}
function cancelAutoSend() {
  if (autoSendTimer) {
    clearTimeout(autoSendTimer);
    autoSendTimer = null;
  }
}

// 双 API 降级写入手机剪贴板。
async function copyToClipboard(text) {
  if (navigator.clipboard && window.isSecureContext) {
    try { await navigator.clipboard.writeText(text); return true; } catch {}
  }
  const ta = document.createElement('textarea');
  ta.value = text;
  ta.style.position = 'fixed';
  ta.style.opacity = '0';
  document.body.appendChild(ta);
  ta.focus();
  ta.select();
  let ok = false;
  try { ok = document.execCommand('copy'); } catch {}
  document.body.removeChild(ta);
  return ok;
}

// PC → 手机 文本
async function onPullText(content) {
  const ok = await copyToClipboard(content);
  if (ok) {
    toast('已复制到手机剪贴板');
  } else {
    // 兜底：写入主 textarea 并全选
    textEl.value = content;
    textEl.focus();
    textEl.select();
    toast('已写入输入框，点复制按钮');
  }
}

// PC → 手机 图片
function onPullImage(b64, mime) {
  modalImg.src = `data:${mime};base64,${b64}`;
  modal.classList.add('show');
}

// 大 base64 分块编码，避免 fromCharCode 栈溢出。
async function fileToB64(file) {
  const buf = await file.arrayBuffer();
  const bytes = new Uint8Array(buf);
  let bin = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}

async function uploadImage(file) {
  if (!file.type.startsWith('image/')) {
    toast('只支持图片');
    return;
  }
  try {
    const b64 = await fileToB64(file);
    if (b64.length > MAX_IMG_B64) {
      toast('图片太大（>10MB）');
      return;
    }
    send({ type: 'set_clipboard_image', data: b64 });
    toast('上传中...');
  } catch (e) {
    toast('读取文件失败');
  }
}

// 按钮绑定
sendBtn.addEventListener('click', sendText);
if (btnTheme) {
  btnTheme.addEventListener('click', toggleTheme);
  // 初始化图标状态（与首屏已 resolve 的 data-theme 对齐）
  updateThemeIcon(resolvedThemeNow());
}
clearBtn.addEventListener('click', () => { cancelAutoSend(); textEl.value = ''; });
textEl.addEventListener('compositionstart', () => { isComposing = true; });
textEl.addEventListener('compositionend', () => {
  isComposing = false;
  scheduleAutoSend();
});
textEl.addEventListener('input', scheduleAutoSend);
btnEnter.addEventListener('click', () => send({ type: 'enter' }));
btnCopy.addEventListener('click', () => send({ type: 'copy' }));
btnPaste.addEventListener('click', () => send({ type: 'paste' }));
pullTextBtn.addEventListener('click', () => send({ type: 'get_clipboard_text' }));
pullImageBtn.addEventListener('click', () => send({ type: 'get_clipboard_image' }));
pickImageBtn.addEventListener('click', () => filePick.click());
filePick.addEventListener('change', () => {
  if (filePick.files && filePick.files[0]) {
    uploadImage(filePick.files[0]);
    filePick.value = '';
  }
});
btnTools.addEventListener('click', () => toolPanel.classList.toggle('open'));
modalClose.addEventListener('click', () => modal.classList.remove('show'));
modal.addEventListener('click', (e) => {
  if (e.target === modal) modal.classList.remove('show');
});
helpBtn.addEventListener('click', () => helpModal.classList.add('show'));
helpClose.addEventListener('click', () => helpModal.classList.remove('show'));
// 配置页链接：直接跳到 /config?t=<token>，省去托盘菜单路径
document.getElementById('help-config-link').href = `/config?t=${encodeURIComponent(token)}`;
helpModal.addEventListener('click', (e) => {
  if (e.target === helpModal) helpModal.classList.remove('show');
});

// ===== 触控板 =====
// 灵敏度：手机屏幕上 1px 拖动 → PC 上 1.5px 光标移动
const TOUCH_SENSITIVITY = 1.5;
// 轻点判定：移动 < 10px 且持续 < 250ms 视为左键点击
const TAP_MAX_DIST = 10;
const TAP_MAX_MS = 250;
// 移动事件最小间隔（30ms ≈ 33fps），避免 WS 消息洪泛
const MOVE_MIN_MS = 30;
// 滚动步长（每次按滚动按钮的滚轮「格」数）
const SCROLL_STEP = 3;
// tap-tap-drag 检测窗口（两次 touch 之间的最大间隔，超过则视为独立点击）
// 注意：单次 tap 会等这段时间才发 click，造成 ~300ms 点击延迟，是双击拖动的代价
const TAP_TAP_GAP_MS = 300;
// 第二次按下后累积移动达多少像素才确认进入 drag（避免轻微抖动误触发）
const DRAG_START_DIST = 5;

btnTouchpad.addEventListener('click', () => {
  syncTouchpadStatus();
  touchpadModal.classList.add('show');
});
touchpadClose.addEventListener('click', () => touchpadModal.classList.remove('show'));

function syncTouchpadStatus() {
  const open = ws && ws.readyState === WebSocket.OPEN;
  touchpadStatus.className = 'touchpad-status ' + (open ? 'connected' : 'disconnected');
  touchpadStatusText.textContent = open ? `${deviceName} 已连接` : '未连接';
}

// 触摸状态机（实现 macOS 风格「双击后按住拖动」）：
//   IDLE           - 没有手指
//   TICKING        - 第一次按下，正在判断是 tap 还是 move
//   MOVING         - 已确认是 move（光标跟随）
//   TAP_PENDING    - 第一次 lift 是 tap；等 TAP_TAP_GAP_MS 看第二次 tap 是否到来
//   POTENTIAL_DRAG - 第二次按下到达（已补发第一次的 click）；等移动 → drag
//   DRAGGING       - drag 中（左键已按下）
const TOUCH_IDLE = 0, TOUCH_TICKING = 1, TOUCH_MOVING = 2,
      TOUCH_TAP_PENDING = 3, TOUCH_POTENTIAL_DRAG = 4, TOUCH_DRAGGING = 5;
let touchState = TOUCH_IDLE;

let touchStartX = 0, touchStartY = 0;
let touchLastX = 0, touchLastY = 0;
let touchStartT = 0;
let touchLastSendT = 0;
let touchMovedDist = 0;
let lastTapEndT = 0;
let pendingClickTimer = null;

function sendMove(dx, dy) {
  const now = Date.now();
  if (now - touchLastSendT < MOVE_MIN_MS) return;
  touchLastSendT = now;
  send({
    type: 'mouse_move',
    dx: Math.round(dx * TOUCH_SENSITIVITY),
    dy: Math.round(dy * TOUCH_SENSITIVITY),
  });
}

touchArea.addEventListener('touchstart', (e) => {
  if (e.touches.length !== 1) return;
  const t = e.touches[0];
  touchStartX = touchLastX = t.clientX;
  touchStartY = touchLastY = t.clientY;
  touchStartT = Date.now();
  touchLastSendT = 0;
  touchMovedDist = 0;

  if (touchState === TOUCH_TAP_PENDING &&
      (touchStartT - lastTapEndT) < TAP_TAP_GAP_MS) {
    // 第二次按下到达：立即补发第一次的 click，然后准备 drag
    if (pendingClickTimer) {
      clearTimeout(pendingClickTimer);
      pendingClickTimer = null;
    }
    send({ type: 'mouse_click', button: 'left' });
    touchState = TOUCH_POTENTIAL_DRAG;
  } else {
    touchState = TOUCH_TICKING;
  }
}, { passive: true });

touchArea.addEventListener('touchmove', (e) => {
  if (e.touches.length !== 1) return;
  e.preventDefault();
  const t = e.touches[0];
  const dx = t.clientX - touchLastX;
  const dy = t.clientY - touchLastY;
  touchLastX = t.clientX;
  touchLastY = t.clientY;
  touchMovedDist += Math.abs(dx) + Math.abs(dy);

  switch (touchState) {
    case TOUCH_TICKING:
      // 第一次 move → 进入光标跟随
      touchState = TOUCH_MOVING;
      sendMove(dx, dy);
      break;
    case TOUCH_MOVING:
      sendMove(dx, dy);
      break;
    case TOUCH_POTENTIAL_DRAG:
      // 累积移动达阈值 → 进入 drag（先按下左键）
      if (touchMovedDist >= DRAG_START_DIST) {
        send({ type: 'mouse_press', button: 'left' });
        touchState = TOUCH_DRAGGING;
        sendMove(dx, dy);
      }
      break;
    case TOUCH_DRAGGING:
      sendMove(dx, dy);
      break;
  }
}, { passive: false });

touchArea.addEventListener('touchend', () => {
  const dt = Date.now() - touchStartT;
  switch (touchState) {
    case TOUCH_TICKING: {
      // 短促且不动 → 视为 tap；等 TAP_TAP_GAP_MS 看是否双击
      if (touchMovedDist < TAP_MAX_DIST && dt < TAP_MAX_MS) {
        touchState = TOUCH_TAP_PENDING;
        lastTapEndT = Date.now();
        if (pendingClickTimer) clearTimeout(pendingClickTimer);
        pendingClickTimer = setTimeout(() => {
          pendingClickTimer = null;
          if (touchState === TOUCH_TAP_PENDING) {
            send({ type: 'mouse_click', button: 'left' });
            touchState = TOUCH_IDLE;
          }
        }, TAP_TAP_GAP_MS);
      } else {
        touchState = TOUCH_IDLE;
      }
      break;
    }
    case TOUCH_MOVING:
      touchState = TOUCH_IDLE;
      break;
    case TOUCH_POTENTIAL_DRAG:
      // 第二次按下后没拖动就抬起 → 这次也算 tap（与补发的 click 合成双击）
      send({ type: 'mouse_click', button: 'left' });
      touchState = TOUCH_IDLE;
      break;
    case TOUCH_DRAGGING:
      send({ type: 'mouse_release', button: 'left' });
      touchState = TOUCH_IDLE;
      break;
  }
});

touchArea.addEventListener('touchcancel', () => {
  // 中断（如来电）时若在 drag，补一次 release 防止左键卡住
  if (touchState === TOUCH_DRAGGING) {
    send({ type: 'mouse_release', button: 'left' });
  }
  if (pendingClickTimer) {
    clearTimeout(pendingClickTimer);
    pendingClickTimer = null;
  }
  touchState = TOUCH_IDLE;
});

btnMouseLeft.addEventListener('click', () => send({ type: 'mouse_click', button: 'left' }));
btnMouseRight.addEventListener('click', () => send({ type: 'mouse_click', button: 'right' }));
btnScrollUp.addEventListener('click', () => send({ type: 'mouse_scroll', dy: -SCROLL_STEP }));
btnScrollDown.addEventListener('click', () => send({ type: 'mouse_scroll', dy: SCROLL_STEP }));

// ===== 文件传输 =====

function formatSize(bytes) {
  if (bytes < 1024) return bytes + ' B';
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + ' KB';
  if (bytes < 1024 * 1024 * 1024) return (bytes / 1024 / 1024).toFixed(1) + ' MB';
  return (bytes / 1024 / 1024 / 1024).toFixed(2) + ' GB';
}

function showTransfer(title, received, total) {
  transferTitle.textContent = title;
  transferProgress.value = total > 0 ? received / total : 0;
  transferStats.textContent = `${formatSize(received)} / ${formatSize(total)}`;
  transferModal.classList.add('show');
}

function hideTransfer() {
  transferModal.classList.remove('show');
}

// 服务端确认上传 → 启动 XHR 上传当前队首文件
function onUploadReady(url) {
  if (!uploadCurrent) {
    // 异常:服务端发了 upload_ready 但前端没在传。忽略。
    return;
  }
  const file = uploadCurrent;
  const xhr = new XMLHttpRequest();
  currentUploadXHR = xhr;

  // 多文件时 title 加 (idx/total) 前缀,聚合进度 = 本文件之前已完成字节 + 本文件已传字节
  const idx = uploadTotalCount - uploadQueue.length; // 1-indexed 当前文件序号
  const titlePrefix = uploadTotalCount > 1 ? `上传 (${idx}/${uploadTotalCount}) ` : '上传 ';
  showTransfer(titlePrefix + file.name, uploadBytesDone, uploadTotalBytes);

  xhr.upload.onprogress = (e) => {
    if (e.lengthComputable) {
      showTransfer(titlePrefix + file.name, uploadBytesDone + e.loaded, uploadTotalBytes);
    }
  };
  xhr.onload = () => {
    currentUploadXHR = null;
    if (xhr.status === 200) {
      uploadBytesDone += file.size;
      // 后端响应里的 name 是实际落盘文件名（重名时加 UUID 后缀）。收集起来，
      // 批结束后用 set_clipboard_files 一次性推剪贴板。
      try {
        const resp = JSON.parse(xhr.responseText);
        if (resp.name) uploadSavedNames.push(resp.name);
      } catch {}
      toast(`已上传 ${file.name}`);
      uploadCurrent = null;
      startNextUpload();
    } else if (xhr.status === 413) {
      toast(`文件太大:${file.name}`);
      abortUploadBatch();
    } else {
      toast(`上传失败 (${xhr.status}):${file.name}`);
      abortUploadBatch();
    }
  };
  xhr.onerror = () => {
    currentUploadXHR = null;
    toast(`上传失败（网络错误）:${file.name}`);
    abortUploadBatch();
  };
  xhr.open('POST', url);
  xhr.send(file);
}

// 从队列取下一个文件发起 upload_start。队列空 → 整批完成,推剪贴板 + 状态归零。
function startNextUpload() {
  if (uploadQueue.length === 0) {
    finishUploadBatch();
    return;
  }
  uploadCurrent = uploadQueue.shift();
  // 先弹 transfer modal 占位,避免用户在 upload_start → upload_ready 之间感觉卡住
  const idx = uploadTotalCount - uploadQueue.length;
  const titlePrefix = uploadTotalCount > 1 ? `上传 (${idx}/${uploadTotalCount}) ` : '上传 ';
  showTransfer(titlePrefix + uploadCurrent.name, uploadBytesDone, uploadTotalBytes);
  send({ type: 'upload_start', name: uploadCurrent.name, size: uploadCurrent.size, mime: uploadCurrent.type || '' });
}

// 整批结束:把收集到的实际落盘文件名一次性 set_clipboard_files 推剪贴板,
// 让 PC 上 Ctrl+V 像在资源管理器里 Ctrl+C 多选文件一样粘出。
function finishUploadBatch() {
  const count = uploadSavedNames.length;
  if (count > 0) {
    send({ type: 'set_clipboard_files', names: uploadSavedNames });
    toast(count === 1 ? '已上传 · 已复制到剪贴板' : `已上传 ${count} 个文件 · 已复制到剪贴板`);
  }
  resetUploadBatch();
  hideTransfer();
}

// 整批中止:中断当前 XHR + 清队列 + 关 modal。失败和用户主动取消都走这条。
function abortUploadBatch() {
  if (currentUploadXHR) {
    currentUploadXHR.abort();
    currentUploadXHR = null;
  }
  uploadCurrent = null;
  resetUploadBatch();
  hideTransfer();
}

function resetUploadBatch() {
  uploadQueue = [];
  uploadBytesDone = 0;
  uploadTotalBytes = 0;
  uploadTotalCount = 0;
  uploadSavedNames = [];
}

// 服务端返回 file 元信息 → 交给浏览器原生下载接管
function onFileMeta(m) {
  const a = document.createElement('a');
  a.href = m.url;
  a.download = m.name;
  a.target = '_blank';
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  toast(`正在下载 ${m.name}…`);
}

// 服务端返回 file_list → 单文件直接下载，多文件弹列表让用户选（含「全部下载」）
function onFileList(files) {
  if (!files || files.length === 0) {
    toast(`${deviceName} 剪贴板为空`);
    return;
  }
  if (files.length === 1) {
    onFileMeta(files[0]);
    return;
  }
  showFilePicker(files);
}

function escapeHtml(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;')
    .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function showFilePicker(files) {
  fileListCurrent = files;
  fileListSelected.clear();
  updateDownloadButton();
  fileListItems.innerHTML = '';
  files.forEach((f, i) => {
    const item = document.createElement('div');
    item.className = 'file-list-item';
    item.innerHTML = `<div class="check"></div><div class="info"><div class="name">${escapeHtml(f.name)}</div><div class="meta">${escapeHtml(formatSize(f.size))} · ${escapeHtml(f.mime || '')}</div></div>`;
    item.addEventListener('click', () => {
      if (fileListSelected.has(i)) {
        fileListSelected.delete(i);
        item.classList.remove('selected');
      } else {
        fileListSelected.add(i);
        item.classList.add('selected');
      }
      updateDownloadButton();
    });
    fileListItems.appendChild(item);
  });
  fileListModal.classList.add('show');
}

function updateDownloadButton() {
  const n = fileListSelected.size;
  fileListDownload.textContent = n > 0 ? `下载 ${n}` : '下载';
  fileListDownload.disabled = n === 0;
}

// 「全部下载」:浏览器对连续程序性下载有限制(iOS Safari 尤其严),用 250ms stagger
// 让每个 <a download> 有足够时间被浏览器下载管理器接管。
fileListDownloadAll.addEventListener('click', () => {
  const files = fileListCurrent;
  hideFilePicker();
  files.forEach((f, i) => {
    setTimeout(() => onFileMeta(f), i * 250);
  });
});

// 「下载 N」:只下载选中的文件,同样 250ms stagger。
fileListDownload.addEventListener('click', () => {
  const selected = [...fileListSelected].sort((a, b) => a - b).map((i) => fileListCurrent[i]);
  if (selected.length === 0) return;
  hideFilePicker();
  selected.forEach((f, i) => {
    setTimeout(() => onFileMeta(f), i * 250);
  });
});

function hideFilePicker() {
  fileListModal.classList.remove('show');
}

fileListClose.addEventListener('click', hideFilePicker);
fileListModal.addEventListener('click', (e) => {
  if (e.target === fileListModal) hideFilePicker();
});

transferCancel.addEventListener('click', () => {
  abortUploadBatch();
});

sendFileBtn.addEventListener('click', () => fileSend.click());
fileSend.addEventListener('change', () => {
  const files = fileSend.files;
  if (!files || files.length === 0) return;
  // ⚠️ 必须先快照再清空 input.value。FileList 是 live 引用,清空 input.value 后
  //    FileList 也被清空,后续 for...of 会迭代 0 次 → 上传根本不启动。
  const snapshot = Array.from(files);
  fileSend.value = '';
  // 批量入队。新批次(队列空 + 没有在传)→ 计数从 0 起;追加到进行中的批次 → 计数累加。
  const wasIdle = uploadQueue.length === 0 && !uploadCurrent && !currentUploadXHR;
  if (wasIdle) {
    uploadBytesDone = 0;
    uploadTotalBytes = 0;
    uploadTotalCount = 0;
  }
  for (const f of snapshot) {
    uploadQueue.push(f);
    uploadTotalBytes += f.size;
    uploadTotalCount += 1;
  }
  // 队列空闲 → 立即发起第一个;否则等当前文件传完 onUploadReady 自动续。
  if (wasIdle) {
    startNextUpload();
  }
});
pullFileBtn.addEventListener('click', () => send({ type: 'get_file' }));

// 粘贴截图监听（绑 document，覆盖任意焦点）
document.addEventListener('paste', async (e) => {
  const items = e.clipboardData?.items || [];
  for (const it of items) {
    if (it.kind === 'file' && it.type.startsWith('image/')) {
      e.preventDefault();
      uploadImage(it.getAsFile());
      return;
    }
  }
});
