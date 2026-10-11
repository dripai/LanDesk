// Keep the latest pending frame while decoding. Session generations prevent a
// late image callback from overwriting a newly connected desktop.
export function createFrames(screen, failed, urls = URL) {
  let pending = null, drawing = false, current = null, decoding = null, generation = 0;
  function reset() {
    generation++; pending = null; drawing = false;
    screen.onload = null; screen.onerror = null; screen.removeAttribute('src');
    if (current) urls.revokeObjectURL(current);
    if (decoding) urls.revokeObjectURL(decoding);
    current = null; decoding = null;
  }
  function draw(blob) {
    if (drawing) { pending = blob; return; }
    drawing = true;
    const version = generation, next = urls.createObjectURL(blob);
    decoding = next;
    screen.onload = () => {
      if (version !== generation) return;
      if (current) urls.revokeObjectURL(current);
      current = next; decoding = null; drawing = false;
      if (pending) { const latest = pending; pending = null; draw(latest); }
    };
    screen.onerror = () => {
      if (version !== generation) return;
      reset(); failed('画面解码失败，请重新连接');
    };
    screen.src = next;
  }
  return {draw, reset};
}
