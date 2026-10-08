import test from 'node:test';
import assert from 'node:assert/strict';
import {panelWidth} from './files.js';

test('panel width clamps dragging and keyboard extremes while retaining desktop space', () => {
  assert.equal(panelWidth(-100, 1920), 240);
  assert.equal(panelWidth(Infinity, 1920), 640);
  assert.equal(panelWidth(355, 1920), 355);
  assert.equal(panelWidth(640, 800), 514);
  assert.equal(panelWidth(640, 320), 154);
  assert.equal(panelWidth(640, 0), 0);
});
