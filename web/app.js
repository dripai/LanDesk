'use strict';
import {createFrames} from './frames.js';
import {createKeyboard} from './keyboard.js';
import {createFiles} from './files.js';
import {createImagePaste} from './clipboard.js';
const $ = id => document.getElementById(id);
const serverName = new URLSearchParams(location.hash.slice(1)).get('name');
if (serverName) document.title = `${serverName} · LanDesk`;
let socket = null, heartbeat = null, lastMove = 0;
const frames = createFrames($('screen'), end);
const send = value => { if (socket?.readyState === WebSocket.OPEN) socket.send(JSON.stringify(value)); };
function notify(message) {
  $('notice').textContent = message; $('notice').hidden = false;
  clearTimeout(notify.timer); notify.timer = setTimeout(() => { $('notice').hidden = true; }, 5000);
}
const imagePaste = createImagePaste(value => {
  if (socket?.readyState !== WebSocket.OPEN) throw new Error('连接已断开');
  send(value);
}, notify);
const keyboard = createKeyboard($('keyboard-input'), send, notify, imagePaste.paste, () => copyClipboard(true));
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
    const items = await navigator.clipboard.read();
    if (!connection || socket !== connection) throw new Error('连接已断开');
    const images = items.filter(item => item.types.some(type => type.startsWith('image/')));
    if (images.length) {
      if (images.length !== 1) throw new Error('每次请粘贴一张图片');
      const type = images[0].types.includes('image/png') ? 'image/png' : images[0].types.find(type => type.startsWith('image/'));
      const blob = await images[0].getType(type);
      if (socket !== connection) throw new Error('连接已断开');
      keyboard.releaseAll(); keyboard.focus(); await imagePaste.paste(blob);
    } else {
      const item = items.find(item => item.types.includes('text/plain'));
      if (!item) throw new Error('剪贴板没有文字或图片；文件请通过右侧面板上传');
      const text = await (await item.getType('text/plain')).text();
      if (socket !== connection) throw new Error('连接已断开');
      keyboard.pasteText(text); keyboard.focus();
    }
  } catch (error) { notify(`无法粘贴：${error.message}`); }
});
function copyClipboard(shortcut = false) {
  if (clipboardRequest || socket?.readyState !== WebSocket.OPEN) return;
  const id = ++clipboardSequence;
  const timer = setTimeout(() => {
    clipboardRequest = null; $('clipboard-copy').disabled = false; notify('读取 Mac 剪贴板超时');
  }, 5000);
  clipboardRequest = {id, timer}; $('clipboard-copy').disabled = true;
  send({type:shortcut ? 'copy_clipboard' : 'read_clipboard',id});
}
$('clipboard-copy').addEventListener('click', () => copyClipboard());
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
  keyboard.clear(); files.reset(); imagePaste.reset();
  if (clipboardRequest) clearTimeout(clipboardRequest.timer);
  clipboardRequest = null; $('clipboard-copy').disabled = false;
  $('notice').hidden = true;
  clearTimeout(resolutionTimer); resolutionTimer = null;
  $('display-apply').disabled = false; $('display-dialog').close(); $('text-dialog').close();
  $('session').hidden=true; $('login').hidden=false;
  $('connect').disabled=false; $('login-status').textContent=message;
  frames.reset();
  if(document.fullscreenElement) document.exitFullscreen().catch(()=>{});
}
function updateDisplay(message) {
  nativeWidth = message.native_width; nativeHeight = message.native_height;
  resolutionWidth = message.requested_width;
  const select = $('display-select'); select.replaceChildren();
  for (const [index, display] of message.displays.entries()) {
    const option = document.createElement('option'); option.value = display.id;
    option.textContent = `${display.virtual ? '虚拟屏幕' : display.main ? '主屏' : `显示器 ${index + 1}`} · ${display.width} × ${display.height}`;
    select.append(option);
  }
  select.value = message.display_id;
  $('resolution-width').max = nativeWidth;
  for (const option of $('resolution-mode').options) if (/^\d+$/.test(option.value)) option.disabled = Number(option.value) > nativeWidth;
  files.resize(message.width / message.height);
}
$('display-select').addEventListener('change', () => {
  releaseAll(); send({type:'set_display', display_id:Number($('display-select').value)});
});
function virtualTarget(prefix) {
  const width = Number($(prefix + 'width').value), height = Number($(prefix + 'height').value);
  if (!Number.isInteger(width) || !Number.isInteger(height) || width <= 0 || height <= 0 || width * height > 16000000) throw new Error('虚拟屏幕宽高需为正整数，且不超过当前采集支持的 1600 万像素');
  return {kind:'virtual',width,height};
}
$('connect-display').addEventListener('change', () => {
  $('connect-virtual-size').hidden = $('connect-display').value !== 'virtual';
});
$('virtual-apply').addEventListener('click', () => {
  try { const display = virtualTarget('virtual-'); releaseAll(); send({type:'set_display_source',display}); }
  catch (error) { notify(error.message); }
});
function connect() {
  if (socket) return;
  let display;
  try {
    if ($('connect-display').value === 'virtual') display = virtualTarget('connect-virtual-');
    else if ($('connect-display').value === 'existing') display = {kind:'existing',id:null};
  } catch (error) { $('login-status').textContent = error.message; return; }
  $('connect').disabled=true; $('login-status').textContent='正在连接…';
  const wsURL = new URL('./ws', location.href); wsURL.protocol = 'ws:'; wsURL.hash = ''; wsURL.search = '';
  socket=new WebSocket(wsURL); socket.binaryType='blob';
  const connection = socket;
  socket.onopen=()=>send(display ? {type:'hello',display} : {type:'hello'});
  socket.onmessage=event=>{
    if (socket !== connection) return;
    if(event.data instanceof Blob) {
      frames.draw(event.data); return;
    }
    const message=JSON.parse(event.data);
    if (message.type === 'notice') { notify(message.message); return; }
    if(imagePaste.handle(message)) return;
    if(files.handle(message)) return;
    if(message.type === 'clipboard_text' || message.type === 'clipboard_error') {
      if(clipboardRequest?.id !== message.id) return;
      const request = clipboardRequest;
      clearTimeout(request.timer);
      const finish = text => {
        if (socket !== connection || clipboardRequest !== request) return;
        clipboardRequest = null; $('clipboard-copy').disabled = false; notify(text);
      };
      if(message.type === 'clipboard_error') { finish(message.message); return; }
      navigator.clipboard.writeText(message.text).then(() => finish('Mac 文字已复制到本机剪贴板')).catch(error => finish(`无法写入本机剪贴板：${error.message}`));
      return;
    }
    if(message.type === 'display_state' || message.type === 'resolution_error') {
      clearTimeout(resolutionTimer); resolutionTimer = null; $('display-apply').disabled = false;
      if(message.type === 'resolution_error') { notify(message.message); return; }
      updateDisplay(message);
      $('resolution-status').textContent = `当前采集 ${message.width} × ${message.height}，等比例显示`;
      $('display-dialog').close(); notify(`采集分辨率：${message.width} × ${message.height}`);
      return;
    }
    if(message.type==='error') { end(message.message); return; }
    if(message.type==='ready') {
      updateDisplay(message);
      $('virtual-display-options').hidden = !message.capabilities.virtual_display;
      $('connect-display').value='current'; $('connect-virtual-size').hidden=true;
      $('login').hidden=true; $('session').hidden=false;
      $('status').textContent='已连接';
      send({type:'heartbeat'}); heartbeat=setInterval(()=>send({type:'heartbeat'}),3000);
      keyboard.focus();

      $('resolution-width').max = nativeWidth;
      for (const option of $('resolution-mode').options) if (/^\d+$/.test(option.value)) option.disabled = Number(option.value) > nativeWidth;
      files.connect(message.width / message.height);
    }

  };
  socket.onerror=()=>end('无法连接，请确认 Mac 应用和 LanDeskClient 或 SSH 隧道都在运行');
  socket.onclose=()=>{ if(socket) end('连接已断开'); };
}
$('connect-form').addEventListener('submit', event => { event.preventDefault(); connect(); });
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

connect();
