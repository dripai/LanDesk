export function createToolbar(areaElement, beforeDrag = () => {}) {
  const $ = id => document.getElementById(id);
  const toolbar = $('toolbar'), toolbarHandle = $('toolbar-handle');
  let toolbarPosition = null, toolbarDrag = null;
  function positionToolbar(x, y) {
    const area = areaElement.getBoundingClientRect();
    const rect = toolbar.getBoundingClientRect();
    if (area.width === 0 || area.height === 0) return;
    x = Math.max(0, Math.min(x, Math.max(0, area.width - rect.width)));
    y = Math.max(0, Math.min(y, Math.max(0, area.height - rect.height)));
    toolbarPosition = {x, y};
    toolbar.style.left = `${x}px`; toolbar.style.top = `${y}px`; toolbar.style.transform = 'none';
  }
  toolbarHandle.addEventListener('pointerdown', event => {
    if (event.button !== 0 || toolbarDrag) return;
    event.preventDefault(); beforeDrag();
    const rect = toolbar.getBoundingClientRect();
    toolbarDrag = {id:event.pointerId, dx:event.clientX - rect.left, dy:event.clientY - rect.top};
    toolbarHandle.setPointerCapture(event.pointerId); toolbar.classList.add('dragging');
  });
  toolbarHandle.addEventListener('pointermove', event => {
    if (!toolbarDrag || event.pointerId !== toolbarDrag.id) return;
    const area = areaElement.getBoundingClientRect();
    positionToolbar(event.clientX - area.left - toolbarDrag.dx, event.clientY - area.top - toolbarDrag.dy);
  });
  function stopToolbarDrag(event) {
    if (!toolbarDrag || event.pointerId !== toolbarDrag.id) return;
    toolbarDrag = null; toolbar.classList.remove('dragging');
  }
  for (const event of ['pointerup', 'pointercancel', 'lostpointercapture']) toolbarHandle.addEventListener(event, stopToolbarDrag);
  toolbarHandle.addEventListener('keydown', event => {
    const moves = {ArrowLeft:[-10,0], ArrowRight:[10,0], ArrowUp:[0,-10], ArrowDown:[0,10]};
    const move = moves[event.key];
    if (!move) return;
    event.preventDefault();
    const area = areaElement.getBoundingClientRect(), rect = toolbar.getBoundingClientRect();
    positionToolbar(rect.left - area.left + move[0], rect.top - area.top + move[1]);
  });
  $('toolbar-toggle').addEventListener('click', () => {
    const collapsed = !$('toolbar-controls').hidden;
    $('toolbar-controls').hidden = collapsed;
    const label = collapsed ? '展开工具条' : '收起工具条';
    $('toolbar-toggle').setAttribute('aria-expanded', String(!collapsed));
    $('toolbar-toggle').setAttribute('aria-label', label); $('toolbar-toggle').title = label;
    $('toolbar-toggle').firstElementChild.textContent = collapsed ? '+' : '−';
  });
  new ResizeObserver(() => {
    if (toolbarPosition) positionToolbar(toolbarPosition.x, toolbarPosition.y);
  }).observe(toolbar);
  window.addEventListener('resize', () => {
    if (toolbarPosition) positionToolbar(toolbarPosition.x, toolbarPosition.y);
  });
}
