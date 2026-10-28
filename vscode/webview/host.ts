// The webview side of the message channel, typed per view, plus tiny DOM helpers (text only goes in via textContent,
// never innerHTML, so database values can't inject markup).
interface VsCodeApi {
  postMessage(message: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
}

declare function acquireVsCodeApi(): VsCodeApi;

export interface Host<In, Out, State> {
  post(message: Out): void;
  listen(handler: (message: In) => void): void;
  state(): Partial<State>;
  save(state: Partial<State>): void;
}

export function host<In, Out, State>(): Host<In, Out, State> {
  const api = acquireVsCodeApi();
  return {
    post: (m) => api.postMessage(m),
    listen: (handler) => window.addEventListener('message', (e: MessageEvent<In>) => handler(e.data)),
    state: () => (api.getState() as Partial<State> | undefined) ?? {},
    save: (s) => api.setState({ ...((api.getState() as object | undefined) ?? {}), ...s }),
  };
}

type Child = Node | string | null | undefined | false;

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: { class?: string; title?: string; text?: string; [attr: string]: string | undefined } = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v === undefined) continue;
    if (k === 'class') e.className = v;
    else if (k === 'text') e.textContent = v;
    else e.setAttribute(k, v);
  }
  for (const c of children) if (c !== null && c !== undefined && c !== false) e.append(c);
  return e;
}

export const SVG_NS = 'http://www.w3.org/2000/svg';

export function svg<K extends keyof SVGElementTagNameMap>(
  tag: K,
  attrs: Record<string, string | number | undefined> = {},
  ...children: (Node | null | undefined)[]
): SVGElementTagNameMap[K] {
  const e = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) if (v !== undefined) e.setAttribute(k, String(v));
  for (const c of children) if (c) e.append(c);
  return e;
}

export function button(label: string, title: string, onClick: () => void, cls = ''): HTMLButtonElement {
  const b = el('button', { class: `btn ${cls}`.trim(), title, text: label, type: 'button' });
  b.addEventListener('click', onClick);
  return b;
}

/** A canvas text measurer for a CSS font string (memoised per font + text, which repeats a lot in grids). */
export function measurer(): (text: string, font: string) => number {
  const ctx = document.createElement('canvas').getContext('2d')!;
  const cache = new Map<string, number>();
  let current = '';
  return (text, font) => {
    const key = font + '\u0000' + text;
    let w = cache.get(key);
    if (w === undefined) {
      if (current !== font) ctx.font = current = font;
      w = ctx.measureText(text).width;
      if (cache.size > 50_000) cache.clear();
      cache.set(key, w);
    }
    return w;
  };
}

export function cssVar(name: string, fallback: string): string {
  return getComputedStyle(document.body).getPropertyValue(name).trim() || fallback;
}

/** A small context menu; closes on Escape, scroll or a click elsewhere. */
export function contextMenu(x: number, y: number, items: ({ label: string; run: () => void; disabled?: boolean } | '-')[]): void {
  document.querySelector('.menu')?.remove();
  const menu = el('div', { class: 'menu', role: 'menu' });
  const close = () => {
    menu.remove();
    window.removeEventListener('mousedown', outside, true);
    window.removeEventListener('keydown', esc, true);
    window.removeEventListener('blur', close);
  };
  const outside = (e: MouseEvent) => !menu.contains(e.target as Node) && close();
  const esc = (e: KeyboardEvent) => e.key === 'Escape' && close();
  for (const item of items) {
    if (item === '-') {
      menu.append(el('div', { class: 'menu-sep' }));
      continue;
    }
    const b = el('button', { class: 'menu-item', role: 'menuitem', text: item.label, type: 'button' });
    b.disabled = !!item.disabled;
    b.addEventListener('click', () => {
      close();
      item.run();
    });
    menu.append(b);
  }
  document.body.append(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, window.innerWidth - r.width - 4)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - r.height - 4)}px`;
  window.addEventListener('mousedown', outside, true);
  window.addEventListener('keydown', esc, true);
  window.addEventListener('blur', close);
}
