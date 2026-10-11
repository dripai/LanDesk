import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import vm from 'node:vm';
import {createKeyboard} from './keyboard.js';

const source = (await readFile(new URL('./app.js', import.meta.url), 'utf8')).replace(/^import .*;\n/gm, '');
function fixture(writeText = async () => {}) {
  const nodes = new Map(), sockets = [], images = [], timers = new Set();
  const node = id => {
    if (!nodes.has(id)) {
      const element = new EventTarget();
      Object.assign(element, {value:'',style:{},hidden:false,options:[],
        focus() { this.dispatchEvent(new Event('focus')); }, close() {},
        replaceChildren() {}, append() {}, getBoundingClientRect:()=>({width:100,height:100}),
      });
      nodes.set(id,element);
    }
    return nodes.get(id);
  };
  class Socket {
    static OPEN = 1;
    readyState = 1; sent = [];
    constructor() { sockets.push(this); }
    send(value) { this.sent.push(JSON.parse(value)); }
    close() { this.readyState = 3; }
    receive(value) { this.onmessage({data:JSON.stringify(value)}); }
  }
  const context = vm.createContext({
    URL, URLSearchParams, Blob, TextEncoder, WebSocket:Socket,
    location:{href:'http://127.0.0.1:17890/s/test/',hash:''},
    document:{getElementById:node,createElement:()=>({})},
    window:new EventTarget(), navigator:{clipboard:{writeText}},
    ResizeObserver:class { observe() {} }, createKeyboard,
    createFrames:()=>({draw() {},reset() {}}),
    createFiles:()=>({handle:()=>false,resize() {},connect() {},reset() {}}),
    createImagePaste:()=>({paste:image=>images.push(image),handle:()=>false,reset() {}}),
    setTimeout:fn=>{ timers.add(fn); return fn; },clearTimeout:fn=>timers.delete(fn),
    setInterval:()=>0,clearInterval() {},
  });
  vm.runInContext(source,context);
  function event(id,type,fields={}) {
    const event = new Event(type,{cancelable:true});
    Object.assign(event,{getModifierState:()=>false,...fields});
    node(id).dispatchEvent(event);
    return event;
  }
  const copy = () => event('keyboard-input','keydown',{key:'c',code:'KeyC',ctrlKey:true});
  return {node,sockets,images,event,copy};
}

test('remote Ctrl+C writes returned text locally; Ctrl+V sends text or image to Mac', async () => {
  const written = [], f = fixture(async text=>written.push(text)), socket=f.sockets[0];
  f.copy();
  const request = socket.sent.at(-1);
  assert.equal(request.type,'copy_clipboard');
  socket.receive({type:'clipboard_text',id:request.id,text:'来自 Mac，hello'});
  await Promise.resolve();
  assert.deepEqual(written,['来自 Mac，hello']);
  assert.equal(f.node('clipboard-copy').disabled,false);
  f.event('keyboard-input','paste',{clipboardData:{items:[],getData:()=>'Windows 文字'}});
  assert.deepEqual(socket.sent.at(-1),{type:'paste_text',text:'Windows 文字'});
  const image = new Blob(['image'],{type:'image/png'});
  f.event('keyboard-input','paste',{clipboardData:{items:[{kind:'file',type:'image/png',getAsFile:()=>image}]}});
  assert.deepEqual(f.images,[image]);
});

test('stale clipboard responses after reconnect cannot write or finish a new request', async () => {
  let finishWrite;
  const written=[], f=fixture(text=>{ written.push(text); return new Promise(resolve=>{finishWrite=resolve;}); });
  const old = f.sockets[0];
  f.copy(); const first=old.sent.at(-1);
  old.receive({type:'clipboard_text',id:first.id,text:'first'});
  f.event('disconnect','click'); f.event('connect-form','submit');
  f.copy();
  old.receive({type:'clipboard_text',id:first.id,text:'late'});
  finishWrite(); await Promise.resolve();
  assert.deepEqual(written,['first']);
  assert.equal(f.node('clipboard-copy').disabled,true);
  const current=f.sockets[1], request=current.sent.at(-1);
  current.receive({type:'clipboard_error',id:request.id,message:'未选中文字'});
  assert.equal(f.node('clipboard-copy').disabled,false);
  assert.equal(f.node('notice').textContent,'未选中文字');
});

test('denied local clipboard permission reports the error without disconnecting', async () => {
  const f=fixture(async()=>{ throw new Error('permission denied'); }), socket=f.sockets[0];
  f.copy();
  socket.receive({type:'clipboard_text',id:socket.sent.at(-1).id,text:'text'});
  await Promise.resolve(); await Promise.resolve();
  assert.match(f.node('notice').textContent,/permission denied/);
  assert.equal(f.node('clipboard-copy').disabled,false);
  assert.equal(socket.readyState,1);
});

test('headless reconnect can request a virtual display before receiving any frame', () => {
  const f=fixture();
  f.sockets[0].onopen();
  assert.deepEqual(f.sockets[0].sent[0],{type:'hello'});
  f.sockets[0].receive({type:'error',message:'没有可采集的显示器'});
  f.node('connect-display').value='virtual';
  f.node('connect-virtual-width').value='1920';
  f.node('connect-virtual-height').value='1080';
  f.event('connect-display','change');
  assert.equal(f.node('connect-virtual-size').hidden,false);
  f.event('connect-form','submit');
  f.sockets[1].onopen();
  assert.deepEqual(f.sockets[1].sent[0],{type:'hello',display:{kind:'virtual',width:1920,height:1080}});
});

test('virtual display failures preserve the current remote session', () => {
  const f=fixture(), socket=f.sockets[0];
  f.node('virtual-width').value='2560'; f.node('virtual-height').value='1440';
  f.event('virtual-apply','click');
  assert.deepEqual(socket.sent.at(-1),{type:'set_display_source',display:{kind:'virtual',width:2560,height:1440}});
  socket.receive({type:'resolution_error',message:'系统拒绝创建'});
  assert.equal(socket.readyState,1);
  assert.equal(f.node('notice').textContent,'系统拒绝创建');
  const count=socket.sent.length;
  f.node('virtual-width').value='0'; f.event('virtual-apply','click');
  assert.equal(socket.sent.length,count);
});
