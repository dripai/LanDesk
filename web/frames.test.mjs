import test from 'node:test';
import assert from 'node:assert/strict';
import {createFrames} from './frames.js';

function fixture() {
  const revoked = [], errors = [];
  const screen = {removeAttribute() { delete this.src; }};
  const frames = createFrames(screen, error => errors.push(error), {
    createObjectURL: blob => blob, revokeObjectURL: url => revoked.push(url),
  });
  return {screen, frames, revoked, errors};
}
test('decode finishes with the latest frame, discarding only intermediate frames', () => {
  const f = fixture();
  f.frames.draw('first'); f.frames.draw('middle'); f.frames.draw('last');
  assert.equal(f.screen.src, 'first');
  f.screen.onload(); assert.equal(f.screen.src, 'last');
  f.screen.onload(); assert.deepEqual(f.revoked, ['first']);
  f.frames.reset(); assert.deepEqual(f.revoked, ['first','last']);
});
test('callbacks from a disconnected session cannot mutate the new frame', () => {
  const f = fixture(); f.frames.draw('old');
  const oldLoad = f.screen.onload, oldError = f.screen.onerror;
  f.frames.reset(); f.frames.draw('new'); oldLoad(); oldError();
  assert.equal(f.screen.src, 'new'); assert.deepEqual(f.errors, []);
  f.screen.onload(); f.frames.reset();
  assert.deepEqual(f.revoked, ['old','new']);
});
