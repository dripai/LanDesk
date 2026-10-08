'use strict';
import {createKeyboard} from './keyboard.js';
import {createFiles} from './files.js';
const $ = id => document.getElementById(id);
$('code').addEventListener('input', () => {
  const input = $('code');
  const caret = input.selectionStart ?? input.value.length;
  const position = Math.min(6, input.value.slice(0, caret).replace(/[^0-9]/g, '').length);
  input.value = input.value.replace(/[^0-9]/g, '').slice(0, 6);
  input.setSelectionRange(position, position);
});
let socket = null, heartbeat = null, imageURL = null, drawing = false, lastMove = 0;
const send = value => { if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value)); };
function notify(message) {
  $('notice').textContent = message; $('notice').hidden = false;
  clearTimeout(notify.timer); notify.timer = setTimeout(() => { $('notice').hidden = true; }, 5000);
}
const keyboard = createKeyboard($('keyboard-input'), send, notify);
const files = createFiles($('files-panel'), value => {
  if (socket?.readyState !== WebSocket.OPEN) throw new Error('连接已断开');
  send(value);
}, data => {
  if (socket?.readyState !== WebSocket.OPEN) throw new Error('连接已断开');
  socket.send(data);
}, notify);
let clipboardRequest = null, clipboardSequence = 0;
let nativeWidth = 0, nativeHeight = 0, resolutionWidth = null, resolutionTimer = null;
$('files-button').addEventListener('click', () => files.toggle());
$('clipboard-paste').addEventListener('click', async () => {
  const connection = socket;
  try {
    const text = await navigator.clipboard.readText();
    if (!connection || socket !== connection) throw new Error('连接已断开');
    if (!text) throw new Error('剪贴板没有文字；不支持图片或文件');
    keyboard.text(text); keyboard.focus();
  } catch (error) { notify(`无法粘贴文字：${error.message}`); }
});
$('clipboard-copy').addEventListener('click', () => {
  if (clipboardRequest) return;
  const id = ++clipboardSequence;
  const timer = setTimeout(() => {
    clipboardRequest = null; $('clipboard-copy').disabled = false; notify('读取 Mac 剪贴板超时');
  }, 5000);
  clipboardRequest = {id, timer}; $('clipboard-copy').disabled = true;
  send({type:'read_clipboard',id});
});
const toolbar = $('toolbar'), toolbarHandle = $('toolbar-handle');
let toolbarPosition = null, toolbarDrag = null;
function positionToolbar(x, y) {
  const area = $('session').getBoundingClientRect();
  const rect = toolbar.getBoundingClientRect();
  if (area.width === 0 || area.height === 0) return;
  x = Math.max(0, Math.min(x, Math.max(0, area.width - rect.width)));
  y = Math.max(0, Math.min(y, Math.max(0, area.height - rect.height)));
  toolbarPosition = {x, y};
  toolbar.style.left = `${x}px`; toolbar.style.top = `${y}px`; toolbar.style.transform = 'none';
}
toolbarHandle.addEventListener('pointerdown', event => {
  if (event.button !== 0 || toolbarDrag) return;
  event.preventDefault(); releaseAll();
  const rect = toolbar.getBoundingClientRect();
  toolbarDrag = {id:event.pointerId, dx:event.clientX - rect.left, dy:event.clientY - rect.top};
  toolbarHandle.setPointerCapture(event.pointerId); toolbar.classList.add('dragging');
});
toolbarHandle.addEventListener('pointermove', event => {
  if (!toolbarDrag || event.pointerId !== toolbarDrag.id) return;
  const area = $('session').getBoundingClientRect();
  positionToolbar(event.clientX - area.left - toolbarDrag.dx, event.clientY - area.top - toolbarDrag.dy);
});
function stopToolbarDrag(event) {
  if (!toolbarDrag || event.pointerId !== toolbarDrag.id) return;
  toolbarDrag = null; toolbar.classList.remove('dragging');
}
for (const event of ['pointerup', 'pointercancel', 'lostpointercapture']) toolbarHandle.addEventListener(event, stopToolbarDrag);
toolbarHandle.addEventListener('keydown', event => {
  const moves = {ArrowLeft:[-10,0], ArrowRight:[10,0], ArrowUp:[0,-10], ArrowDown:[0,10]};
  const move = moves[event.key];
  if (!move) return;
  event.preventDefault();
  const area = $('session').getBoundingClientRect(), rect = toolbar.getBoundingClientRect();
  positionToolbar(rect.left - area.left + move[0], rect.top - area.top + move[1]);
});
$('toolbar-toggle').addEventListener('click', () => {
  const collapsed = !$('toolbar-controls').hidden;
  $('toolbar-controls').hidden = collapsed;
  const label = collapsed ? '展开工具条' : '收起工具条';
  $('toolbar-toggle').setAttribute('aria-expanded', String(!collapsed));
  $('toolbar-toggle').setAttribute('aria-label', label); $('toolbar-toggle').title = label;
  $('toolbar-toggle').firstElementChild.textContent = collapsed ? '+' : '−';
});
new ResizeObserver(() => {
  if (toolbarPosition) positionToolbar(toolbarPosition.x, toolbarPosition.y);
}).observe(toolbar);
window.addEventListener('resize', () => {
  if (toolbarPosition) positionToolbar(toolbarPosition.x, toolbarPosition.y);
});
function releaseAll() { keyboard.releaseAll(); }
function end(message) {
  clearInterval(heartbeat); heartbeat=null;
  if(socket) { const old=socket; socket=null; old.onclose=null; old.onerror=null; old.close(); }
  keyboard.clear(); files.reset();
  if (clipboardRequest) clearTimeout(clipboardRequest.timer);
  clipboardRequest = null; $('clipboard-copy').disabled = false;
  $('notice').hidden = true;
  clearTimeout(resolutionTimer); resolutionTimer = null;
  $('display-apply').disabled = false; $('display-dialog').close(); $('text-dialog').close();
  $('session').hidden=true; $('login').hidden=false;
  $('connect').disabled=false; $('login-status').textContent=message;
  if(imageURL) { URL.revokeObjectURL(imageURL); imageURL=null; }
  $('screen').removeAttribute('src');
  if(document.fullscreenElement) document.exitFullscreen().catch(()=>{});
}
$('connect-form').addEventListener('submit', event => {
  event.preventDefault(); $('connect').disabled=true; $('login-status').textContent='正在连接…';
  socket=new WebSocket(`ws://${location.host}/ws`); socket.binaryType='blob';
  socket.onopen=()=>send({type:'hello',code:$('code').value});
  socket.onmessage=event=>{
    if(event.data instanceof Blob) {
      if(drawing) return;
      drawing=true; const next=URL.createObjectURL(event.data);
      $('screen').onload=()=>{ if(imageURL) URL.revokeObjectURL(imageURL); imageURL=next; drawing=false; };
      $('screen').onerror=()=>{ URL.revokeObjectURL(next); drawing=false; end('画面解码失败，请重新连接'); };
      $('screen').src=next; return;
    }
    const message=JSON.parse(event.data);
    if(files.handle(message)) return;
    if(message.type === 'clipboard_text' || message.type === 'clipboard_error') {
      if(clipboardRequest?.id !== message.id) return;
      clearTimeout(clipboardRequest.timer); clipboardRequest = null; $('clipboard-copy').disabled = false;
      if(message.type === 'clipboard_error') { notify(message.message); return; }
      navigator.clipboard.writeText(message.text).then(() => notify('Mac 文字已复制到本机剪贴板')).catch(error => notify(`无法写入本机剪贴板：${error.message}`));
      return;
    }
    if(message.type === 'resolution' || message.type === 'resolution_error') {
      clearTimeout(resolutionTimer); resolutionTimer = null; $('display-apply').disabled = false;
      if(message.type === 'resolution_error') { notify(message.message); return; }
      resolutionWidth = message.requested_width;
      $('resolution-status').textContent = `当前采集 ${message.width} × ${message.height}，等比例显示`;
      $('display-dialog').close(); notify(`采集分辨率：${message.width} × ${message.height}`);
      return;
    }
    if(message.type==='error') { end(message.message); return; }
    if(message.type==='ready') {
      $('login').hidden=true; $('session').hidden=false; $('code').value='';
      $('status').textContent='已连接'; drawing=false;
      send({type:'heartbeat'}); heartbeat=setInterval(()=>send({type:'heartbeat'}),3000);
      keyboard.focus();
      nativeWidth = message.width; nativeHeight = message.height; resolutionWidth = null;
      $('resolution-width').max = nativeWidth;
      for (const option of $('resolution-mode').options) if (/^\d+$/.test(option.value)) option.disabled = Number(option.value) > nativeWidth;
      files.connect(message.width / message.height);
    }

  };
  socket.onerror=()=>end('无法连接，请确认 Mac 应用和 SSH 连接脚本都在运行');
  socket.onclose=()=>{ if(socket) end('连接已断开'); };
});
$('disconnect').addEventListener('click',()=>{ releaseAll(); send({type:'disconnect'}); end('已断开'); });
$('fullscreen').addEventListener('click',()=>{ const p=document.fullscreenElement?document.exitFullscreen():$('session').requestFullscreen(); p.catch(()=>{}); });
function pointer(event) {
  const rect=$('screen').getBoundingClientRect();
  if(rect.width===0 || rect.height===0) return false;
  const x=(event.clientX-rect.left)/rect.width,y=(event.clientY-rect.top)/rect.height;
  if(x<0||x>1||y<0||y>1) return false;
  send({type:'pointer',x,y}); return true;
}
$('screen').addEventListener('pointermove',e=>{ const now=performance.now(); if(now-lastMove>=30) { pointer(e); lastMove=now; } });
$('screen').addEventListener('pointerdown',e=>{ e.preventDefault(); const rect=$('viewport').getBoundingClientRect(); keyboard.focus(e.clientX-rect.left,e.clientY-rect.top); if(pointer(e)) { $('screen').setPointerCapture(e.pointerId); send({type:'button',button:e.button,down:true}); } });
$('screen').addEventListener('pointerup',e=>{ e.preventDefault(); pointer(e); send({type:'button',button:e.button,down:false}); });
$('screen').addEventListener('pointercancel',releaseAll);
$('screen').addEventListener('contextmenu',e=>e.preventDefault());
$('screen').addEventListener('wheel',e=>{ e.preventDefault(); send({type:'wheel',x:Math.max(-10,Math.min(10,Math.round(e.deltaX/40))),y:Math.max(-10,Math.min(10,Math.round(e.deltaY/40)))}); },{passive:false});
window.addEventListener('blur',releaseAll);
window.addEventListener('pagehide',()=>{ releaseAll(); send({type:'disconnect'}); socket?.close(); });
$('text-button').addEventListener('click',()=>{ releaseAll(); $('text-dialog').showModal(); $('text').focus(); });
$('text-cancel').addEventListener('click',()=>$('text-dialog').close());
$('text-form').addEventListener('submit',e=>{ e.preventDefault(); keyboard.text($('text').value); $('text').value=''; $('text-dialog').close(); keyboard.focus(); });
$('display-button').addEventListener('click', () => {
  releaseAll();
  const presets = ['1280','1920','2560'];
  $('resolution-mode').value = resolutionWidth === null ? 'native' : presets.includes(String(resolutionWidth)) ? String(resolutionWidth) : 'custom';
  $('resolution-width').value = resolutionWidth ?? nativeWidth;
  $('resolution-width').hidden = $('resolution-mode').value !== 'custom';
  const width = resolutionWidth ?? nativeWidth, height = Math.round(nativeHeight * width / nativeWidth);
  $('resolution-status').textContent = `当前采集 ${width} × ${height}，等比例显示；Mac 系统分辨率不变。`;
  $('display-dialog').showModal();
});
$('resolution-mode').addEventListener('change', () => { $('resolution-width').hidden = $('resolution-mode').value !== 'custom'; });
$('display-cancel').addEventListener('click', () => $('display-dialog').close());
$('display-form').addEventListener('submit', event => {
  event.preventDefault();
  const mode = $('resolution-mode').value;
  const width = mode === 'native' ? null : Number(mode === 'custom' ? $('resolution-width').value : mode);
  if (width !== null && (!Number.isInteger(width) || width < 640 || width > nativeWidth)) { notify(`采集宽度必须为 640–${nativeWidth} 像素`); return; }
  $('display-apply').disabled = true;
  resolutionTimer = setTimeout(() => { resolutionTimer = null; $('display-apply').disabled = false; notify('分辨率切换超时，实际采集状态尚未确认'); }, 10000);
  send({type:'set_resolution',width});
});
