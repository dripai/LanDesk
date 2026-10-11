const CHUNK_BYTES = 65536, MAX_FILE_BYTES = 512 * 1024 * 1024;

export function panelWidth(requested, available) {
  const maximum = Math.max(0, Math.min(640, available - Math.min(280, available * 0.5) - 6));
  return Math.round(Math.max(Math.min(240, maximum), Math.min(requested, maximum)));
}

function icon(kind) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 24 24'); svg.setAttribute('aria-hidden', 'true');
  svg.classList.add('entry-icon', kind === 'directory' ? 'folder-icon' : 'document-icon');
  const shape = document.createElementNS(svg.namespaceURI, 'path');
  shape.setAttribute('d', kind === 'directory' ? 'M3 6h7l2 3h9v11H3zM3 6V4h7l2 2h9v3' : 'M5 3h9l5 5v13H5zM14 3v6h5M8 13h8M8 17h6');
  svg.append(shape); return svg;
}

export function createFiles(panel, send, sendBinary, notify) {
  const $ = id => panel.querySelector(`#${id}`);
  const pending = new Map();
  let sequence = 0, latestList = 0, path = '', connected = false, uploading = false, generation = 0, choice = null;
  let imageAspect = 1.6, chosenWidth = null, drag = null, nextCursor = null, directoryCount = 0, fileCount = 0;
  const resizer = document.getElementById('files-resizer');
  function wait(id, expected, action) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(id); reject(new Error('文件操作超时，请刷新目录确认结果')); }, 15000);
      pending.set(id, {expected, resolve, reject, timer});
      try { action(); } catch (error) { clearTimeout(timer); pending.delete(id); reject(error); }
    });
  }
  function handle(message) {
    if (!['directory','file_error','upload_ready','upload_progress','upload_done','upload_cancelled'].includes(message.type)) return false;
    const request = pending.get(message.id);
    if (!request) return true;
    if (message.type !== 'file_error' && message.type !== request.expected) return true;
    clearTimeout(request.timer); pending.delete(message.id);
    if (message.type === 'file_error') {
      const error = new Error(message.message); error.uploadActive = message.upload_active;
      request.reject(error);
    } else request.resolve(message);
    return true;
  }
  function reset() {
    connected = false; generation++; latestList++; uploading = false; path = ''; choice = null;
    for (const request of pending.values()) { clearTimeout(request.timer); request.reject(new Error('连接已断开')); }
    pending.clear(); nextCursor = null; $('file-more').hidden = true; $('directories').replaceChildren(); $('file-items').replaceChildren(); $('upload-status').textContent = '';
    $('upload-files').disabled = false; $('file-picker').value = ''; panel.hidden = true; resizer.hidden = true;
    $('upload-progress').hidden = true; drag = null; resizer.classList.remove('dragging');
    panel.parentElement.style.removeProperty('--remote-width');
  }
  function formatSize(size) {
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KiB`;
    return `${(size / (1024 * 1024)).toFixed(1)} MiB`;
  }
  async function list(next = path, append = false) {
    if (!connected) return;
    const id = ++sequence; latestList = id;
    $('file-refresh').disabled = true; $('file-more').disabled = true;
    try {
      const result = await wait(id, 'directory', () => send({type:'list_directory',id,path:next,cursor:append ? nextCursor : ''}));
      if (id !== latestList) return;
      path = result.path;
      $('file-path').replaceChildren();
      $('file-path').title = result.root + (path ? '/' + path : '');
      const segments = path ? path.split('/') : [];
      [result.root.split('/').pop(), ...segments].forEach((name, index) => {
        const crumb = document.createElement('button'); crumb.type = 'button'; crumb.textContent = name;
        crumb.title = index ? segments.slice(0, index).join('/') : result.root;
        crumb.disabled = index === segments.length;
        crumb.addEventListener('click', () => list(segments.slice(0, index).join('/')));
        if (index) { const slash = document.createElement('span'); slash.textContent = '/'; $('file-path').append(slash); }
        $('file-path').append(crumb);
      });
      $('file-up').disabled = !path;
      if (!append) { $('directories').replaceChildren(); $('file-items').replaceChildren(); directoryCount = 0; fileCount = 0; }
      for (const empty of panel.querySelectorAll('.files-empty')) empty.remove();
      for (const entry of result.entries) {
        if (entry.kind === 'directory') {
          const button = document.createElement('button'); button.type = 'button'; button.className = 'directory-item';
          const name = document.createElement('span'); name.textContent = entry.name;
          const arrow = document.createElement('span'); arrow.className = 'directory-arrow'; arrow.textContent = '›';
          button.append(icon('directory'), name, arrow); button.title = entry.name;
          button.addEventListener('click', () => list(path ? path + '/' + entry.name : entry.name));
          $('directories').append(button); directoryCount++;
        } else {
          const row = document.createElement('div'); row.className = 'file-item';
          const label = document.createElement('span'); label.textContent = entry.name; label.title = entry.name;
          const detail = document.createElement('small'); detail.textContent = entry.kind === 'file' ? formatSize(entry.size) : entry.kind === 'symlink' ? '链接' : '特殊文件';
          row.append(icon(entry.kind), label, detail); $('file-items').append(row); fileCount++;
        }
      }
      $('directory-count').textContent = directoryCount;
      $('file-count').textContent = fileCount;
      nextCursor = result.next_cursor; $('file-more').hidden = !nextCursor;
      for (const [id, message] of [['directories', '此目录没有子目录'], ['file-items', '此目录没有文件']]) {
        if (!$(id).childElementCount) { const empty = document.createElement('div'); empty.className = 'files-empty'; empty.textContent = message; $(id).append(empty); }
      }
    } catch (error) { if (connected && id === latestList) notify(error.message); }
    finally { if (id === latestList) { $('file-refresh').disabled = false; $('file-more').disabled = false; } }
  }
  async function upload(files) {
    if (!connected || !files.length) return;
    if (uploading) { notify('请等待当前上传完成'); return; }
    uploading = true; $('upload-files').disabled = true;
    const destination = path, version = generation;
    for (const file of files) {
      if (!connected || version !== generation) break;
      const id = ++sequence;
      let started = false;
      try {
        if (file.size > MAX_FILE_BYTES) throw new Error(`${file.name} 超过 512 MiB`);
        $('upload-status').textContent = `${file.name} · 0%`;
        $('upload-progress').hidden = false; $('upload-progress').value = 0;
        await wait(id, 'upload_ready', () => send({type:'upload_start',id,path:destination,name:file.name,size:file.size}));
        started = true;
        if (!connected || version !== generation) throw new Error('连接已断开');
        for (let offset = 0; offset < file.size; offset += CHUNK_BYTES) {
          const data = await file.slice(offset, offset + CHUNK_BYTES).arrayBuffer();
          if (!connected || version !== generation) throw new Error('连接已断开');
          const progress = await wait(id, 'upload_progress', () => sendBinary(data));
          $('upload-status').textContent = `${file.name} · ${Math.round(progress.written / file.size * 100)}%`;
          $('upload-progress').value = progress.written / file.size * 100;
        }
        if (!connected || version !== generation) throw new Error('连接已断开');
        await wait(id, 'upload_done', () => send({type:'upload_finish',id}));
        started = false;
        $('upload-status').textContent = `${file.name} · 已上传`;
        $('upload-progress').value = 100;
      } catch (error) {
        if (connected && version === generation) {
          // The server also cleans up automatically on a write error or disconnect.
          if (started && error.uploadActive !== false) {
            try { await wait(id, 'upload_cancelled', () => send({type:'upload_cancel',id})); }
            catch (cleanup) { notify(`${error.message}；${cleanup.message}`); }
          }
          $('upload-status').textContent = `${file.name} · ${error.message}`;
          notify(error.message);
        }
        break;
      }
    }
    if (version === generation) { uploading = false; $('upload-files').disabled = false; $('upload-progress').hidden = true; await list(); }
  }
  function resize(aspect = imageAspect) {
    imageAspect = aspect;
    const session = panel.parentElement;
    const gap = Math.max(0, (session.clientWidth - Math.min(session.clientWidth, session.clientHeight * imageAspect)) / 2);
    const visible = choice ?? gap >= 180;
    panel.hidden = !connected || !visible;
    resizer.hidden = panel.hidden;
    const width = panelWidth(chosenWidth ?? (gap >= 180 ? Math.min(320, gap) : 280), session.clientWidth);
    panel.style.width = `${width}px`;
    session.style.setProperty('--remote-width', `${session.clientWidth - (panel.hidden ? 0 : width + 6)}px`);
    resizer.setAttribute('aria-valuenow', width);
    resizer.setAttribute('aria-valuemin', panelWidth(0, session.clientWidth));
    resizer.setAttribute('aria-valuemax', panelWidth(Infinity, session.clientWidth));
    document.getElementById('files-button').setAttribute('aria-pressed', String(!panel.hidden));
  }
  resizer.addEventListener('pointerdown', event => {
    if (event.button !== 0) return;
    event.preventDefault(); send({type:'release_all'});
    drag = {x:event.clientX, width:panel.getBoundingClientRect().width};
    resizer.setPointerCapture(event.pointerId); resizer.classList.add('dragging'); resizer.focus();
  });
  resizer.addEventListener('pointermove', event => {
    if (!drag) return;
    chosenWidth = panelWidth(drag.width + drag.x - event.clientX, panel.parentElement.clientWidth); resize();
  });
  for (const type of ['pointerup', 'pointercancel', 'lostpointercapture']) resizer.addEventListener(type, () => { drag = null; resizer.classList.remove('dragging'); });
  resizer.addEventListener('keydown', event => {
    if (!['ArrowLeft','ArrowRight','Home','End'].includes(event.key)) return;
    event.preventDefault();
    const width = panel.getBoundingClientRect().width;
    chosenWidth = panelWidth(event.key === 'Home' ? 0 : event.key === 'End' ? Infinity : width + (event.key === 'ArrowLeft' ? 20 : -20), panel.parentElement.clientWidth);
    resize();
  });
  $('file-more').addEventListener('click', () => list(path, true));
  $('file-up').addEventListener('click', () => list(path.split('/').slice(0,-1).join('/')));
  $('file-refresh').addEventListener('click', () => list());
  $('upload-files').addEventListener('click', () => $('file-picker').click());
  $('file-picker').addEventListener('change', event => { upload([...event.target.files]); event.target.value = ''; });
  const drop = $('file-drop');
  drop.addEventListener('dragover', event => { event.preventDefault(); event.dataTransfer.dropEffect = uploading ? 'none' : 'copy'; drop.classList.add('drag-over'); });
  drop.addEventListener('dragleave', event => { if (!drop.contains(event.relatedTarget)) drop.classList.remove('drag-over'); });
  drop.addEventListener('drop', event => {
    event.preventDefault(); drop.classList.remove('drag-over');
    // Chrome/Edge expose dragged directories as entries; reject them explicitly.
    const items = [...event.dataTransfer.items].filter(item => item.kind === 'file');
    if (items.some(item => typeof item.webkitGetAsEntry !== 'function')) {
      notify('浏览器无法识别拖入的文件类型，请使用上传按钮'); return;
    }
    if (items.some(item => !item.webkitGetAsEntry()?.isFile)) {
      notify('只支持普通文件，请先打包目录再上传'); return;
    }
    upload([...event.dataTransfer.files]);
  });
  window.addEventListener('resize', () => resize());
  return {handle, reset, resize, connect(aspect) { connected = true; resize(aspect); list(''); }, toggle() { choice = panel.hidden; resize(); }};
}
