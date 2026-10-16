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

// Directory navigation is async: never upload into the previous directory while
// the clicked destination is still loading.
test('upload waits for the directory response before accepting a destination', async () => {
  const {createFiles} = await import('./files.js');
  class Node extends EventTarget {
    constructor() { super(); this.hidden = false; this.disabled = false; this.children = []; this.style = {setProperty(){},removeProperty(){}}; this.classList = {add(){},remove(){}}; }
    setAttribute() {}
    append(...nodes) { this.children.push(...nodes); }
    replaceChildren(...nodes) { this.children = nodes; }
    get childElementCount() { return this.children.length; }
    getBoundingClientRect() { return {width:280}; }
    setPointerCapture() {}
    focus() {}
    querySelectorAll() { return []; }
  }
  const nodes = new Map();
  const node = id => { if (!nodes.has(id)) nodes.set(id,new Node()); return nodes.get(id); };
  const panel = new Node();
  panel.querySelector = selector => node(selector.slice(1));
  panel.parentElement = Object.assign(new Node(),{clientWidth:1200,clientHeight:720});
  const previousDocument = globalThis.document, previousWindow = globalThis.window;
  globalThis.document = {getElementById:node,createElement:()=>new Node(),createElementNS:()=>new Node()};
  globalThis.window = new EventTarget();
  const sent=[], notices=[];
  let releases = 0;
  const files = createFiles(panel, message=>sent.push(message), ()=>{}, message=>notices.push(message), {initialVisible:true,releaseInput:()=>releases++});
  try {
    files.connect();
    assert.equal(node('upload-files').disabled,true);
    node('file-picker').files = [{name:'test.txt',size:0}];
    node('file-picker').dispatchEvent(new Event('change'));
    assert.equal(sent.length,1);
    assert.match(notices[0],/目录正在加载/);
    files.handle({type:'directory',id:sent[0].id,root:'/',path:'tmp/new-dir',entries:[],next_cursor:null});
    await Promise.resolve();
    assert.equal(node('upload-files').disabled,false);
    const drag = new Event('pointerdown'); Object.assign(drag,{button:0,clientX:100,pointerId:1});
    node('files-resizer').dispatchEvent(drag);
    assert.equal(releases,1);
    assert.ok(!sent.some(message=>message.type==='release_all'));
    node('file-picker').dispatchEvent(new Event('change'));
    assert.equal(sent[1].type,'upload_start');
    assert.equal(sent[1].path,'tmp/new-dir');
  } finally {
    files.reset();
    await Promise.resolve();
    globalThis.document=previousDocument;globalThis.window=previousWindow;
  }
});
