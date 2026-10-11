const MAX_TEXT_BYTES = 65536;
const special = new Set(['Control','Meta','Alt','Shift','Enter','Escape','Tab','Backspace','Delete','ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Home','End','PageUp','PageDown','F1','F2','F3','F4','F5','F6','F7','F8','F9','F10','F11','F12']);
const repeatable = new Set(['ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Backspace','Delete','Enter','Tab','Home','End','PageUp','PageDown']);

export function createKeyboard(input, send, notify, pasteImage, copyText = null) {
  const held = new Map();
  let composing = false, focused = false;
  function clear() { held.clear(); composing = false; input.value = ''; }
  function releaseAll() { clear(); send({type:'release_all'}); }
  function text(value, type = 'text') {
    if (!value) return;
    if (new TextEncoder().encode(value).length > MAX_TEXT_BYTES || value.includes('\0')) {
      notify('文字不能超过 64 KiB，或包含空字符'); return;
    }
    send({type,text:value});
  }
  function commit() {
    const value = input.value; input.value = '';
    if (focused) text(value);
  }
  input.addEventListener('focus', () => { focused = true; });
  input.addEventListener('blur', () => { focused = false; releaseAll(); });
  input.addEventListener('compositionstart', () => { releaseAll(); composing = true; });
  input.addEventListener('compositionend', event => {
    composing = false;
    if (event.data) commit(); else input.value = '';
  });
  input.addEventListener('input', event => { if (!composing && !event.isComposing) commit(); });
  input.addEventListener('paste', event => {
    event.preventDefault();
    const images = [...(event.clipboardData.items || [])].filter(item => item.kind === 'file' && item.type.startsWith('image/'));
    if (images.length) {
      input.value = ''; releaseAll();
      if (images.length !== 1) { notify('每次请粘贴一张图片'); return; }
      pasteImage(images[0].getAsFile()); return;
    }
    const value = event.clipboardData.getData('text/plain');
    if (value) { releaseAll(); text(value, 'paste_text'); } else notify('剪贴板没有文字或图片；文件请通过右侧面板上传');
    input.value = '';
  });
  input.addEventListener('keydown', event => {
    if (composing || event.isComposing || event.key === 'Process' || event.key === 'Dead') return;
    if (copyText && event.ctrlKey && !event.altKey && !event.metaKey && event.code === 'KeyC') {
      event.preventDefault();
      if (!event.repeat) { releaseAll(); copyText(); }
      return;
    }
    // Let the browser deliver Windows clipboard text through the paste event.
    if (event.ctrlKey && !event.altKey && !event.metaKey && event.code === 'KeyV') return;
    const shortcut = (event.ctrlKey || event.metaKey || event.altKey) && !event.getModifierState('AltGraph');
    if (!special.has(event.key) && (!shortcut || [...event.key].length !== 1)) return;
    event.preventDefault();
    if (event.repeat) {
      const key = held.get(event.code);
      if (!repeatable.has(key)) return;
      // Re-press only this editing key; preserve held modifiers and keep native key
      // bookkeeping bounded. Ignore late repeats after blur/release_all.
      send({type:'key',key,down:false});
      send({type:'key',key,down:true});
      return;
    }
    held.set(event.code, event.key); send({type:'key',key:event.key,down:true});
  });
  input.addEventListener('keyup', event => {
    const key = held.get(event.code);
    if (!key) return;
    event.preventDefault(); held.delete(event.code); send({type:'key',key,down:false});
  });
  return {clear, releaseAll, text, pasteText(value) { releaseAll(); text(value, 'paste_text'); }, focus(x = 0, y = 0) {
    input.style.left = `${Math.max(0, x)}px`; input.style.top = `${Math.max(0, y)}px`;
    input.focus({preventScroll:true});
  }};
}
