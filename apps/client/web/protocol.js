export function size(cols, rows) {
  return { cols: Math.max(2, Math.min(500, cols)), rows: Math.max(1, Math.min(300, rows)) };
}

export function inputChunks(bytes) {
  const chunks = [];
  for (let offset = 0; offset < bytes.length; offset += 4096) chunks.push(bytes.subarray(offset, offset + 4096));
  return chunks;
}

// Terminal Ctrl+C is SIGINT; use the familiar terminal copy shortcut instead.
export function shortcut(event) {
  if (event.type !== 'keydown' || !event.ctrlKey || !event.shiftKey || event.altKey || event.metaKey) return null;
  if (event.code === 'KeyC') return 'copy';
  if (event.code === 'KeyV') return 'paste';
  return null;
}
