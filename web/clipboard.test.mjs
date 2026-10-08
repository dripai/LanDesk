import test from 'node:test';
import assert from 'node:assert/strict';
import {createImagePaste, MAX_IMAGE_BYTES} from './clipboard.js';

test('image chunks preserve bytes and finish only after final acknowledgement', async () => {
  const source = new Uint8Array(150001).map((_,index) => index % 251);
  const chunks = [], notices = [];
  const paste = createImagePaste(message => {
    if (message.type !== 'paste_image_chunk') return;
    chunks.push(message);
    const size = Buffer.from(message.data,'base64').length;
    paste.handle({type:message.offset + size === message.total ? 'image_pasted' : 'image_progress', id:message.id});
  }, value => notices.push(value));
  await paste.paste(new Blob([source],{type:'image/png'}));
  assert.deepEqual(chunks.map(value => value.offset), [0,65536,131072]);
  assert.deepEqual(Buffer.concat(chunks.map(value => Buffer.from(value.data,'base64'))), Buffer.from(source));
  assert.match(notices.at(-1), /粘贴图片/);
});

test('reset aborts a pending paste and never sends remaining bytes to a new session', async () => {
  const sent = [];
  let started;
  const ready = new Promise(resolve => { started = resolve; });
  const paste = createImagePaste(message => { sent.push(message); if(message.type === 'paste_image_chunk') started(); }, () => {});
  const result = paste.paste(new Blob([new Uint8Array(100000)],{type:'image/png'}));
  await ready; paste.reset(); await result;
  assert.equal(sent.filter(value => value.type === 'paste_image_chunk').length,1);
  assert.equal(sent.some(value => value.type === 'paste_image_cancel'),false);
});

test('oversized images are rejected locally and server errors cancel without reporting success', async () => {
  const notices = [], sent = [];
  const paste = createImagePaste(message => {
    sent.push(message);
    if (message.type === 'paste_image_chunk') paste.handle({type:'image_error',id:message.id,message:'PNG 内容损坏'});
  }, value => notices.push(value));
  await paste.paste(new Blob([new Uint8Array(MAX_IMAGE_BYTES+1)],{type:'image/png'}));
  assert.equal(sent.some(value => value.type === 'paste_image_chunk'),false);
  assert.match(notices.at(-1), /10 MiB/);
  await paste.paste(new Blob(['bad'],{type:'image/png'}));
  assert.equal(notices.at(-1),'PNG 内容损坏');
  assert.equal(sent.at(-1).type,'paste_image_cancel');
});
