import { mkdir, copyFile } from 'node:fs/promises';

await mkdir(new URL('./vendor/', import.meta.url), { recursive: true });
for (const [source, name] of [
  ['@xterm/xterm/lib/xterm.js', 'xterm.js'],
  ['@xterm/xterm/css/xterm.css', 'xterm.css'],
  ['@xterm/xterm/LICENSE', 'xterm.LICENSE'],
  ['@xterm/addon-fit/lib/addon-fit.js', 'addon-fit.js'],
  ['@xterm/addon-fit/LICENSE', 'addon-fit.LICENSE'],
]) {
  await copyFile(new URL(`./node_modules/${source}`, import.meta.url), new URL(`./vendor/${name}`, import.meta.url));
}
