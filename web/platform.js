export const PROTOCOL_VERSION = 1;
const capabilities = ['capture', 'input', 'clipboard_text', 'clipboard_image', 'files', 'capture_resize', 'display_sleep'];

export function serverInfo(value) {
  if (!value || value.protocol_version !== PROTOCOL_VERSION) throw new Error('服务端协议版本不匹配，请更新 LanDesk 并刷新页面');
  if (!['macos', 'windows', 'linux'].includes(value.os)) throw new Error('服务端返回了未知操作系统');
  for (const key of capabilities) if (typeof value.capabilities?.[key] !== 'boolean') throw new Error(`服务端未报告能力：${key}`);
  if (!value.capabilities.capture || !value.capabilities.input) throw new Error('当前服务端不能提供桌面采集和控制');
  return {...value, label: {macos: 'Mac', windows: 'Windows', linux: 'Linux'}[value.os]};
}
