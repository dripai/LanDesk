import test from 'node:test';
import assert from 'node:assert/strict';
import { size, inputChunks, shortcut } from './protocol.js';

test('large UTF-8 paste remains byte-exact across bounded input messages', () => {
  const bytes = new TextEncoder().encode('中文🙂\r\n'.repeat(3000));
  const chunks = inputChunks(bytes);
  assert.ok(chunks.every(chunk => chunk.length <= 4096));
  assert.deepEqual(Buffer.concat(chunks), Buffer.from(bytes));
});
test('Ctrl+C remains available to the shell and clipboard shortcuts are explicit', () => {
  assert.equal(shortcut({ type: 'keydown', ctrlKey: true, code: 'KeyC' }), null);
  assert.equal(shortcut({ type: 'keydown', ctrlKey: true, shiftKey: true, code: 'KeyC' }), 'copy');
  assert.equal(shortcut({ type: 'keydown', ctrlKey: true, shiftKey: true, code: 'KeyV' }), 'paste');
  assert.equal(shortcut({ type: 'keyup', ctrlKey: true, shiftKey: true, code: 'KeyV' }), null);
});
test('hidden and oversized viewports produce legal PTY dimensions', () => {
  assert.deepEqual(size(0, 0), { cols: 2, rows: 1 });
  assert.deepEqual(size(1000, 1000), { cols: 500, rows: 300 });
});
