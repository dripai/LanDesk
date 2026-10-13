import { size, inputChunks, shortcut } from './protocol.js';

const status = document.querySelector('#status');
const reconnect = document.querySelector('#reconnect');
const disconnect = document.querySelector('#disconnect');
const token = document.querySelector('meta[name="session-token"]').content;
const label = new URLSearchParams(location.hash.slice(1)).get('name') || 'SSH 终端';
document.querySelector('#host').textContent = label;
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
  fit.fit();
  const bounded = size(term.cols, term.rows);
  if (term.cols !== bounded.cols || term.rows !== bounded.rows) term.resize(bounded.cols, bounded.rows);
}
function control(value, target = socket) {
  if (target?.readyState === WebSocket.OPEN) target.send(JSON.stringify(value));
}
function input(bytes) {
  if (!ready || socket?.readyState !== WebSocket.OPEN) return;
  // Bound queued paste/input instead of silently dropping part of a command.
  if (socket.bufferedAmount + bytes.length > 512 * 1024) {
    status.textContent = '输入队列已满，本次输入未发送，请稍后重试';
    return;
  }
  for (const chunk of inputChunks(bytes)) socket.send(chunk);
}
term.onData(data => input(encoder.encode(data)));
term.onBinary(data => input(Uint8Array.from(data, char => char.charCodeAt(0))));
term.onResize(({ cols, rows }) => { if (ready) control({ type: 'resize', ...size(cols, rows) }); });
term.attachCustomKeyEventHandler(event => {
  const action = shortcut(event);
  if (!action) return true;
  event.preventDefault();
  if (action === 'copy') {
    navigator.clipboard.writeText(term.getSelection()).catch(() => { status.textContent = '复制失败，请允许剪贴板访问'; });
  } else {
    const current = generation;
    navigator.clipboard.readText().then(text => {
      if (current === generation && ready) term.paste(text);
    }).catch(() => { status.textContent = '粘贴失败，请允许剪贴板访问或使用浏览器粘贴'; });
  }
  return false;
});
function connect() {
  const current = ++generation;
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
  status.textContent = '正在创建终端…';
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
      status.textContent = '已连接';
      control({ type: 'resize', ...size(term.cols, term.rows) }, ws);
      term.focus();
    } else if (message.type === 'exit' || message.type === 'error') {
      ended = true;
      ready = false;
      term.options.disableStdin = true;
      status.textContent = message.type === 'error' ? message.message : `Shell 已退出${message.code === undefined ? '' : `（${message.code}）`}${message.signal ? `：${message.signal}` : ''}`;
    }
  };
  ws.onerror = () => {
    if (current !== generation) return;
    ended = true;
    status.textContent = '连接失败，请检查 LanDeskClient 中的 SSH 连接';
  };
  ws.onclose = () => {
    if (current !== generation) return;
    ready = false;
    term.options.disableStdin = true;
    reconnect.disabled = false;
    disconnect.disabled = true;
    if (!ended) status.textContent = '已断开；重新连接将创建新的 Shell';
  };
}
disconnect.addEventListener('click', () => {
  ready = false;
  term.options.disableStdin = true;
  control({ type: 'disconnect' });
  socket?.close();
});
// Reload to obtain the new credential if the desktop client reconnected SSH.
reconnect.addEventListener('click', () => location.reload());
new ResizeObserver(fitTerminal).observe(document.querySelector('#terminal'));
window.addEventListener('pagehide', () => { ++generation; socket?.close(); });
connect();
