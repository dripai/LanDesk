import { createFiles } from './files.js';
import { createToolbar } from './toolbar.js';
import { size, inputChunks, shortcut } from './protocol.js';

const status = document.querySelector('#status');
const reconnect = document.querySelector('#reconnect');
const disconnect = document.querySelector('#disconnect');
const token = document.querySelector('meta[name="session-token"]').content;
const label = new URLSearchParams(location.hash.slice(1)).get('name') || 'SSH 终端';
document.querySelector('#toolbar').title = label;
document.title = `${label} · LanDesk 终端`;
const term = new window.Terminal({
  fontSize: 14, fontFamily: 'Consolas, Menlo, monospace', scrollback: 5000,
  cursorBlink: true, disableStdin: true,
  theme: { background: '#13171d', foreground: '#e4e8ef', cursor: '#8ac4ff' },
});
const fit = new window.FitAddon.FitAddon();
term.loadAddon(fit);
term.open(document.querySelector('#terminal'));
let socket = null;
let ready = false;
let generation = 0;
const encoder = new TextEncoder();

function fitTerminal() {
  const container = document.querySelector('#terminal');
  if (!container.clientWidth || !container.clientHeight) return;
  const atBottom = term.buffer.active.viewportY >= term.buffer.active.baseY;
  fit.fit();
  const bounded = size(term.cols, term.rows);
  if (term.cols !== bounded.cols || term.rows !== bounded.rows) term.resize(bounded.cols, bounded.rows);
  if (atBottom) term.scrollToBottom();
}
function control(value, target = socket) {
  if (target?.readyState === WebSocket.OPEN) target.send(JSON.stringify(value));
}
function input(bytes) {
  if (!ready || socket?.readyState !== WebSocket.OPEN) return;
  // Bound queued paste/input instead of silently dropping part of a command.
  if (socket.bufferedAmount + bytes.length > 512 * 1024) {
    notify('输入队列已满，本次输入未发送，请稍后重试');
    return;
  }
  for (const chunk of inputChunks(bytes)) socket.send(chunk);
}
term.onData(data => input(encoder.encode(data)));
term.onBinary(data => input(Uint8Array.from(data, char => char.charCodeAt(0))));
term.onResize(({ cols, rows }) => { if (ready) control({ type: 'resize', ...size(cols, rows) }); });
function notify(message) {
  const notice = document.querySelector('#notice');
  notice.textContent = message; notice.hidden = !message;
  clearTimeout(notify.timer);
  if (message) notify.timer = setTimeout(() => { notice.hidden = true; }, 7000);
}
function setStatus(message, state) {
  status.textContent = message;
  document.querySelector('#toolbar').title = `${label} · ${message}`;
  document.querySelector('#connection-dot').dataset.state = state;
  if (state === 'closed') notify(message);
}
async function copySelection() {
  try { await navigator.clipboard.writeText(term.getSelection()); }
  catch { notify('复制失败，请允许剪贴板访问'); }
}
async function pasteClipboard() {
  const current = generation;
  try {
    const text = await navigator.clipboard.readText();
    if (current === generation && ready) { term.paste(text); term.focus(); }
  } catch { notify('粘贴失败，请允许剪贴板访问或使用浏览器粘贴'); }
}
term.attachCustomKeyEventHandler(event => {
  const action = shortcut(event);
  if (!action) return true;
  event.preventDefault();
  if (action === 'copy') copySelection(); else pasteClipboard();
  return false;
});
document.querySelector('#clipboard-copy').addEventListener('click', copySelection);
document.querySelector('#clipboard-paste').addEventListener('click', pasteClipboard);
document.querySelector('#fullscreen').addEventListener('click', async () => {
  try { if (document.fullscreenElement) await document.exitFullscreen(); else await document.documentElement.requestFullscreen(); }
  catch { notify('浏览器未允许全屏'); }
});
createToolbar(document.querySelector('#session'));
let fileSocket = null;
const files = createFiles(document.querySelector('#files-panel'), message => {
  if (message.type === 'release_all') return;
  if (fileSocket?.readyState !== WebSocket.OPEN) throw new Error('文件连接已断开');
  fileSocket.send(JSON.stringify(message));
}, bytes => {
  if (fileSocket?.readyState !== WebSocket.OPEN) throw new Error('文件连接已断开');
  fileSocket.send(bytes);
}, notify, {initialVisible: true});
document.querySelector('#files-button').addEventListener('click', () => files.toggle());
function closeFiles() {
  const old = fileSocket; fileSocket = null; old?.close(); files.reset();
  document.querySelector('#files-button').disabled = true;
}
function connectFiles(current) {
  closeFiles();
  const url = new URL('files-ws', location.href); url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'; url.hash = '';
  const ws = new WebSocket(url); fileSocket = ws;
  ws.onopen = () => { if (current === generation && fileSocket === ws) ws.send(JSON.stringify({type:'hello',token})); };
  ws.onmessage = event => {
    if (current !== generation || fileSocket !== ws) return;
    const message = JSON.parse(event.data);
    if (message.type === 'files_ready') { document.querySelector('#files-button').disabled = false; files.connect(); }
    else if (message.type === 'error') notify(`文件面板：${message.message}`);
    else files.handle(message);
  };
  ws.onclose = () => {
    if (current !== generation || fileSocket !== ws) return;
    closeFiles(); if (ready) notify('文件连接已关闭，刷新页面可重连；终端仍可使用');
  };
}
function connect() {
  const current = ++generation;
  closeFiles();
  ready = false;
  term.options.disableStdin = true;
  socket?.close();
  term.reset();
  fitTerminal();
  const url = new URL('ws', location.href);
  url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  url.hash = '';
  const ws = new WebSocket(url);
  socket = ws;
  ws.binaryType = 'arraybuffer';
  reconnect.disabled = true;
  disconnect.disabled = false;
  setStatus('正在创建终端…', 'connecting');
  let ended = false;
  ws.onopen = () => {
    if (current !== generation) return;
    control({ type: 'hello', token, ...size(term.cols, term.rows) }, ws);
  };
  ws.onmessage = event => {
    if (current !== generation) return;
    if (event.data instanceof ArrayBuffer) {
      const bytes = new Uint8Array(event.data);
      term.write(bytes, () => {
        if (current === generation) control({ type: 'ack', bytes: bytes.length }, ws);
      });
      return;
    }
    const message = JSON.parse(event.data);
    if (message.type === 'ready') {
      ready = true;
      term.options.disableStdin = false;
      setStatus('已连接', 'connected');
      connectFiles(current);
      control({ type: 'resize', ...size(term.cols, term.rows) }, ws);
      term.focus();
    } else if (message.type === 'exit' || message.type === 'error') {
      ended = true;
      ready = false;
      term.options.disableStdin = true;
      setStatus(message.type === 'error' ? message.message : `Shell 已退出${message.code === undefined ? '' : `（${message.code}）`}${message.signal ? `：${message.signal}` : ''}`, 'closed');
      closeFiles();
    }
  };
  ws.onerror = () => {
    if (current !== generation) return;
    ended = true;
    setStatus('连接失败，请检查 LanDeskClient 中的 SSH 连接', 'closed');
  };
  ws.onclose = () => {
    if (current !== generation) return;
    ready = false;
    term.options.disableStdin = true;
    reconnect.disabled = false;
    disconnect.disabled = true;
    closeFiles();
    if (!ended) setStatus('已断开；重新连接将创建新的 Shell', 'closed');
  };
}
disconnect.addEventListener('click', () => {
  closeFiles();
  ready = false;
  term.options.disableStdin = true;
  control({ type: 'disconnect' });
  socket?.close();
});
// Reload to obtain the new credential if the desktop client reconnected SSH.
reconnect.addEventListener('click', () => location.reload());
let fitFrame = null;
function scheduleFit() { cancelAnimationFrame(fitFrame); fitFrame = requestAnimationFrame(fitTerminal); }
new ResizeObserver(scheduleFit).observe(document.querySelector('#terminal'));
window.visualViewport?.addEventListener('resize', scheduleFit);
document.fonts.ready.then(scheduleFit);
document.addEventListener('fullscreenchange', scheduleFit);
window.addEventListener('pagehide', () => { ++generation; closeFiles(); socket?.close(); });
connect();
