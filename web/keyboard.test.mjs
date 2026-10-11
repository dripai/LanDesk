import test from 'node:test';
import assert from 'node:assert/strict';
import {createKeyboard} from './keyboard.js';

function fixture(pasteImage = () => {}) {
  const field = new EventTarget(); field.value = ''; field.style = {};
  field.focus = () => field.dispatchEvent(new Event('focus'));
  const messages = [], notices = [];
  const keyboard = createKeyboard(field, value => messages.push(value), value => notices.push(value), pasteImage);
  const event = (name, fields = {}) => {
    const event = new Event(name, {cancelable:true});
    Object.assign(event, {getModifierState:() => false, ...fields});
    field.dispatchEvent(event); return event;
  };
  keyboard.focus();
  return {field, messages, notices, keyboard, event};
}

test('half-width and full-width punctuation uses text rather than layout-dependent keys', () => {
  const f = fixture();
  for (const text of [',', '，', '。', '!', '开发中文']) {
    const key = f.event('keydown', {code:'Comma',key:text});
    assert.equal(key.defaultPrevented, false);
    f.field.value = text; f.event('input', {isComposing:false});
  }
  assert.deepEqual(f.messages, [',','，','。','!','开发中文'].map(text => ({type:'text',text})));
  assert.equal(f.field.value, '');
});

test('image paste takes precedence over text and releases remote modifiers', () => {
  const images = [], image = new Blob(['png'], {type:'image/png'});
  const f = fixture(value => images.push(value));
  f.event('keydown', {key:'Control',code:'ControlLeft',ctrlKey:true});
  const event = f.event('paste', {clipboardData:{items:[{kind:'file',type:'image/png',getAsFile:()=>image}],getData:()=>'image URL'}});
  assert.equal(event.defaultPrevented,true);
  assert.deepEqual(images,[image]);
  assert.equal(f.messages.at(-1).type,'release_all');
  assert.equal(f.messages.some(value=>value.type==='text'),false);
});

test('IME commits once, ignores intermediate input and discards cancellation', () => {
  const f = fixture();
  f.event('compositionstart'); f.field.value = 'ni'; f.event('input', {isComposing:true});
  f.event('keydown', {key:'Process',code:'Comma',isComposing:true});
  assert.equal(f.messages.filter(m => m.type === 'text').length, 0);
  f.field.value = '你好，'; f.event('input', {isComposing:true});
  f.event('compositionend', {data:'你好，'}); f.event('input', {isComposing:false});
  assert.deepEqual(f.messages.filter(m => m.type === 'text'), [{type:'text',text:'你好，'}]);
  f.event('compositionstart'); f.field.value = 'cancel'; f.event('compositionend', {data:''});
  f.event('input', {isComposing:false});
  assert.equal(f.messages.filter(m => m.type === 'text').length, 1);
});

test('shortcuts are key pairs, Ctrl+V uses only plain clipboard text', () => {
  const f = fixture();
  f.event('keydown', {key:'Control',code:'ControlLeft',ctrlKey:true});
  f.event('keydown', {key:'c',code:'KeyC',ctrlKey:true});
  f.event('keyup', {key:'c',code:'KeyC'});
  assert.equal(f.event('keydown', {key:'v',code:'KeyV',ctrlKey:true}).defaultPrevented, false);
  let format;
  f.event('paste', {clipboardData:{getData(type) { format = type; return 'text\nonly'; }}});
  assert.equal(format, 'text/plain');
  assert.deepEqual(f.messages.filter(m => m.type === 'paste_text'), [{type:'paste_text',text:'text\nonly'}]);
  assert.equal(f.messages.at(-2).type, 'release_all');
  assert.equal(f.messages.some(m => m.key === 'v'), false);
  assert.deepEqual(f.messages.filter(m => m.key === 'c'), [{type:'key',key:'c',down:true},{type:'key',key:'c',down:false}]);
  f.event('paste', {clipboardData:{getData() { return ''; }}});
  assert.match(f.notices.at(-1), /文件请通过右侧面板/);
});

test('focus loss releases held keys and prevents stray text commits', () => {
  const f = fixture();
  f.event('keydown', {key:'Shift',code:'ShiftLeft'});
  f.event('blur');
  f.event('keyup', {key:'Shift',code:'ShiftLeft'});
  f.field.value = 'stray'; f.event('input', {isComposing:false});
  assert.deepEqual(f.messages, [{type:'key',key:'Shift',down:true},{type:'release_all'}]);
});

test('holding each arrow repeats movement and releases it on keyup', () => {
  for (const key of ['ArrowLeft','ArrowRight','ArrowUp','ArrowDown']) {
    const f = fixture();
    f.event('keydown', {key,code:key});
    for (let i = 0; i < 3; i++) f.event('keydown', {key,code:key,repeat:true});
    f.event('keyup', {key,code:key});
    assert.deepEqual(f.messages, Array.from({length:4}, () => [
      {type:'key',key,down:true}, {type:'key',key,down:false},
    ]).flat());
  }
});

test('arrow repeat preserves modifiers and stops after focus loss', () => {
  const f = fixture();
  f.event('keydown', {key:'Shift',code:'ShiftLeft'});
  f.event('keydown', {key:'Shift',code:'ShiftLeft',repeat:true});
  f.event('keydown', {key:'ArrowRight',code:'ArrowRight',shiftKey:true});
  f.event('keydown', {key:'ArrowRight',code:'ArrowRight',shiftKey:true,repeat:true});
  assert.deepEqual(f.messages, [
    {type:'key',key:'Shift',down:true},
    {type:'key',key:'ArrowRight',down:true},
    {type:'key',key:'ArrowRight',down:false},
    {type:'key',key:'ArrowRight',down:true},
  ]);
  f.event('blur');
  const before = [...f.messages];
  f.event('keydown', {key:'ArrowRight',code:'ArrowRight',repeat:true});
  f.event('keyup', {key:'ArrowRight',code:'ArrowRight'});
  assert.equal(before.at(-1).type, 'release_all');
  assert.deepEqual(f.messages, before);
});

test('text limits count UTF-8 bytes and reject null characters', () => {
  const f = fixture();
  f.keyboard.text('汉'.repeat(22000)); f.keyboard.text('a\0b');
  assert.equal(f.messages.length, 0); assert.equal(f.notices.length, 2);
  f.keyboard.text('a'.repeat(65536)); assert.equal(f.messages[0].text.length, 65536);
});

test('editing keys repeat while modifiers do not', () => {
  for (const key of ['Backspace','Delete','Enter','Tab','Home','End','PageUp','PageDown']) {
    const f = fixture();
    f.event('keydown', {key,code:key});
    f.event('keydown', {key,code:key,repeat:true});
    f.event('keyup', {key,code:key});
    assert.deepEqual(f.messages.map(m => m.down), [true,false,true,false], key);
  }
  const f = fixture();
  f.event('keydown', {key:'Control',code:'ControlLeft'});
  f.event('keydown', {key:'Control',code:'ControlLeft',repeat:true});
  assert.equal(f.messages.length, 1);
});

test('Ctrl+C requests a remote copy once and Ctrl+V remains a local paste event', () => {
  const field = new EventTarget(); field.value=''; field.style={}; field.focus=()=>{};
  const messages=[], copies=[];
  createKeyboard(field, m=>messages.push(m), ()=>{}, ()=>{}, ()=>copies.push('copy'));
  function key(code, repeat = false) {
    const event = new Event('keydown',{cancelable:true});
    Object.assign(event,{code,key:code.slice(-1).toLowerCase(),ctrlKey:true,repeat,getModifierState:()=>false});
    field.dispatchEvent(event); return event;
  }
  assert.equal(key('KeyC').defaultPrevented, true);
  key('KeyC', true); assert.equal(copies.length, 1);
  assert.deepEqual(messages, [{type:'release_all'}]);
  assert.equal(key('KeyV').defaultPrevented, false);
});
