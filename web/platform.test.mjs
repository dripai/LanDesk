import test from 'node:test';
import assert from 'node:assert/strict';
import {serverInfo} from './platform.js';
const capabilities = {capture:true,input:true,clipboard_text:true,clipboard_image:true,files:true,capture_resize:true,display_sleep:false};
test('server handshake selects platform and preserves unsupported capabilities', () => {
  for (const [os,label] of [['macos','Mac'],['windows','Windows']]) {
    const info=serverInfo({protocol_version:1,os,capabilities});
    assert.equal(info.label,label); assert.equal(info.capabilities.display_sleep,false);
  }
});
test('missing capabilities and incompatible protocol fail explicitly', () => {
  assert.throws(()=>serverInfo({protocol_version:2,os:'windows',capabilities}),/版本/);
  assert.throws(()=>serverInfo({protocol_version:1,os:'windows',capabilities:{}}),/未报告/);
  assert.throws(()=>serverInfo({protocol_version:1,os:'unknown',capabilities}),/未知/);
});
