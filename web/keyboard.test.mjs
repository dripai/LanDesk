import test from 'node:test';
import assert from 'node:assert/strict';
import {createKeyboard} from './keyboard.js';

function fixture() {
  const field = new EventTarget(); field.value = ''; field.style = {};
  field.focus = () => field.dispatchEvent(new Event('focus'));
  const messages = [], notices = [];
  const keyboard = createKeyboard(field, value => messages.push(value), value => notices.push(value));
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
  assert.deepEqual(f.messages.filter(m => m.type === 'text'), [{type:'text',text:'text\nonly'}]);
  assert.equal(f.messages.some(m => m.key === 'v'), false);
  assert.deepEqual(f.messages.filter(m => m.key === 'c'), [{type:'key',key:'c',down:true},{type:'key',key:'c',down:false}]);
  f.event('paste', {clipboardData:{getData() { return ''; }}});
  assert.match(f.notices.at(-1), /不支持粘贴图片/);
});

test('focus loss releases held keys and prevents stray text commits', () => {
  const f = fixture();
  f.event('keydown', {key:'Shift',code:'ShiftLeft'});
  f.event('blur');
  f.event('keyup', {key:'Shift',code:'ShiftLeft'});
  f.field.value = 'stray'; f.event('input', {isComposing:false});
  assert.deepEqual(f.messages, [{type:'key',key:'Shift',down:true},{type:'release_all'}]);
});

test('text limits count UTF-8 bytes and reject null characters', () => {
  const f = fixture();
  f.keyboard.text('汉'.repeat(22000)); f.keyboard.text('a\0b');
  assert.equal(f.messages.length, 0); assert.equal(f.notices.length, 2);
  f.keyboard.text('a'.repeat(65536)); assert.equal(f.messages[0].text.length, 65536);
});
