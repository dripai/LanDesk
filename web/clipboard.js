export const MAX_IMAGE_BYTES = 10 * 1024 * 1024;
const CHUNK_BYTES = 65536;

async function pngBlob(blob) {
  if (!blob || !blob.type.startsWith('image/')) throw new Error('剪贴板没有图片');
  if (!blob.size || blob.size > MAX_IMAGE_BYTES) throw new Error('图片不能超过 10 MiB');
  if (blob.type === 'image/png') return blob;
  const bitmap = await createImageBitmap(blob);
  try {
    if (bitmap.width > 8192 || bitmap.height > 8192 || bitmap.width * bitmap.height > 16000000) throw new Error('图片不能超过 1600 万像素或边长 8192');
    const canvas = document.createElement('canvas');
    canvas.width = bitmap.width; canvas.height = bitmap.height;
    canvas.getContext('2d').drawImage(bitmap, 0, 0);
    const png = await new Promise(resolve => canvas.toBlob(resolve, 'image/png'));
    if (!png || png.size > MAX_IMAGE_BYTES) throw new Error('图片转为 PNG 后超过 10 MiB');
    return png;
  } finally { bitmap.close(); }
}

function encode(bytes) {
  let value = '';
  for (let offset = 0; offset < bytes.length; offset += 8192) value += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
  return btoa(value);
}

export function createImagePaste(send, notify) {
  let sequence = 0, generation = 0, busy = false, pending = null;
  function wait(id, expected, message) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending = null; reject(new Error('图片粘贴超时，请重新粘贴')); }, 15000);
      pending = {id, expected, resolve, reject, timer};
      try { send(message); } catch (error) { clearTimeout(timer); pending = null; reject(error); }
    });
  }
  function handle(message) {
    if (!['image_progress', 'image_pasted', 'image_error'].includes(message.type)) return false;
    if (pending?.id !== message.id) return true;
    const request = pending; pending = null; clearTimeout(request.timer);
    if (message.type === 'image_error') request.reject(new Error(message.message));
    else if (message.type !== request.expected) request.reject(new Error('图片传输响应不一致'));
    else request.resolve();
    return true;
  }
  function reset() {
    generation++; busy = false;
    if (pending) { clearTimeout(pending.timer); pending.reject(new Error('连接已断开')); pending = null; }
  }
  async function paste(blob) {
    if (busy) { notify('上一张图片仍在粘贴'); return; }
    busy = true;
    const version = generation, id = ++sequence;
    try {
      const png = await pngBlob(blob);
      const bytes = new Uint8Array(await png.arrayBuffer());
      if (version !== generation) return;
      send({type:'release_all'});
      notify('正在粘贴图片…');
      for (let offset = 0; offset < bytes.length; offset += CHUNK_BYTES) {
        if (version !== generation) return;
        const end = Math.min(offset + CHUNK_BYTES, bytes.length);
        await wait(id, end === bytes.length ? 'image_pasted' : 'image_progress', {
          type:'paste_image_chunk', id, offset, total:bytes.length, data:encode(bytes.subarray(offset,end)),
        });
      }
      if (version === generation) notify('已向 Mac 当前应用粘贴图片');
    } catch (error) {
      if (version === generation) {
        try { send({type:'paste_image_cancel',id}); } catch { /* Connection error already reported below. */ }
        notify(error.message);
      }
    } finally { if (version === generation) busy = false; }
  }
  return {paste, handle, reset};
}
