// Dragging things into order, with a mouse, a finger or a pen (pointer
// events: HTML drag and drop doesn't work on touch screens).
//
// `sortable(root, opts)` lets the `opts.item` elements inside `root` be
// dragged into place within, and between, the `opts.containers` elements.
// The item itself moves as the pointer does, so the page shows where it
// will land; a copy follows the pointer. On release, `opts.onDrop(item,
// container, index)` says where it went (index among the container's
// items), and the caller redraws from its own model. Escape, or the
// browser taking the pointer back, puts it back where it was.
//
// With a mouse or pen, or on a `.drag-handle`, dragging starts once the
// pointer moves a little. Elsewhere on a touch screen it needs a long press,
// so a swipe still scrolls the page.
//
// opts: { item, containers (selectors), handle (selector: only drag from
// there), axis ("y" for a column; else a grid or a row), near(el) (the
// container to use when over `el`, outside any), onDrop, onCancel, onStart }

const MOVE = 5; // px before a mouse drag starts
const SLOP = 10; // px a finger may move during the long press
const PRESS = 280; // ms for a long press
const EDGE = 70; // px from the window's top or bottom where it scrolls

let active = null;
// Whether anything is being dragged now (redraws wait until it's dropped).
export const dragging = () => !!active;

const reduceMotion = () => matchMedia("(prefers-reduced-motion: reduce)").matches;

// Where an element is laid out, ignoring transforms (animations), so the
// drop position doesn't jitter while things slide.
function laidOut(el) {
  const p = el.offsetParent;
  const base = p ? p.getBoundingClientRect() : { left: 0, top: 0 };
  const left = base.left + el.offsetLeft - (p?.scrollLeft ?? 0);
  const top = base.top + el.offsetTop - (p?.scrollTop ?? 0);
  return { left, top, right: left + el.offsetWidth, bottom: top + el.offsetHeight, width: el.offsetWidth, height: el.offsetHeight };
}

// Slide `els` from where they were (`before`, a Map of rects) to where they are.
function slide(els, before) {
  if (reduceMotion()) return;
  for (const el of els) {
    const a = before.get(el);
    if (!a) continue;
    const b = laidOut(el);
    const dx = a.left - b.left, dy = a.top - b.top;
    if (!dx && !dy) continue;
    el.animate([{ transform: `translate(${dx}px, ${dy}px)` }, { transform: "none" }],
      { duration: 160, easing: "cubic-bezier(.2,.7,.3,1)" });
  }
}

export function sortable(root, opts) {
  const axis = opts.axis ?? "grid";
  const itemsOf = (c, except) => [...c.children].filter((x) => x.matches(opts.item) && x !== except);

  // Held from the start (not added on pointerdown: browsers decide at
  // touchstart whether a touch can be kept from scrolling), it keeps the
  // page still under a finger that's dragging.
  root.addEventListener("touchmove", (ev) => { if (active?.on && active.root === root && ev.cancelable) ev.preventDefault(); }, { passive: false });

  root.addEventListener("pointerdown", (e) => {
    if (active || e.button !== 0 || !e.isPrimary) return;
    const item = e.target.closest(opts.item);
    if (!item || !root.contains(item)) return;
    // Only from its own handle, if it has one; never from a field.
    if (opts.handle && e.target.closest(opts.handle)?.closest(opts.item) !== item) return;
    if (e.target.closest("input, select, textarea, a, [data-no-drag]")) return;
    const onHandle = !!e.target.closest(opts.handle ?? ".drag-handle");
    start(e, item, onHandle);
  });

  function start(e, item, onHandle) {
    const touch = e.pointerType === "touch";
    const d = {
      item, root, x0: e.clientX, y0: e.clientY, x: e.clientX, y: e.clientY, id: e.pointerId,
      on: false, timer: null, ghost: null, dx: 0, dy: 0, frame: 0,
      home: { parent: item.parentElement, next: item.nextElementSibling },
    };
    active = d;
    if (touch && !onHandle) {
      d.timer = setTimeout(() => begin(d), PRESS);
    }
    const move = (ev) => {
      if (ev.pointerId !== d.id) return;
      d.x = ev.clientX;
      d.y = ev.clientY;
      const far = Math.hypot(d.x - d.x0, d.y - d.y0);
      if (!d.on) {
        if (d.timer) {
          // A swipe, not a long press: let the page scroll.
          if (far > SLOP) end(false);
        } else if (far > MOVE) {
          begin(d);
        }
        return;
      }
      ev.preventDefault();
      follow(d);
    };
    // While dragging, a finger mustn't scroll the page.
    const touchMove = (ev) => { if (d.on || (touch && onHandle)) ev.preventDefault(); };
    const up = (ev) => { if (ev.pointerId === d.id) end(d.on); };
    const cancel = (ev) => { if (ev.pointerId === d.id) end(false); };
    const key = (ev) => { if (ev.key === "Escape" && d.on) { ev.preventDefault(); ev.stopPropagation(); end(false); } };
    // A long press would open the menu on phones.
    const menu = (ev) => { if (d.timer || d.on) ev.preventDefault(); };
    const lost = () => end(false);
    window.addEventListener("pointermove", move, { passive: false });
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    window.addEventListener("touchmove", touchMove, { passive: false });
    window.addEventListener("keydown", key, true);
    window.addEventListener("contextmenu", menu, true);
    window.addEventListener("blur", lost);
    d.cleanup = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
      window.removeEventListener("touchmove", touchMove);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("contextmenu", menu, true);
      window.removeEventListener("blur", lost);
    };
  }

  function begin(d) {
    clearTimeout(d.timer);
    d.timer = null;
    if (!d.item.isConnected) return end(false);
    d.on = true;
    const r = d.item.getBoundingClientRect();
    d.dx = d.x0 - r.left;
    d.dy = d.y0 - r.top;
    const ghost = d.item.cloneNode(true);
    ghost.classList.add("drag-ghost");
    ghost.removeAttribute("id");
    ghost.setAttribute("aria-hidden", "true");
    Object.assign(ghost.style, { width: `${r.width}px`, height: `${r.height}px`, left: "0", top: "0" });
    // In a modal dialog, the copy must be in it to show above it.
    (d.item.closest("dialog") ?? document.body).append(ghost);
    d.ghost = ghost;
    d.item.classList.add("drag-placeholder");
    document.body.classList.add("is-dragging");
    navigator.vibrate?.(8);
    opts.onStart?.(d.item);
    place(d);
    tick(d);
  }

  // The copy follows the pointer; the item moves to where it would land.
  function follow(d) {
    if (d.frame) return;
    d.frame = requestAnimationFrame(() => {
      d.frame = 0;
      if (active !== d) return;
      place(d);
      reorder(d);
    });
  }

  function place(d) {
    d.ghost.style.transform = `translate(${d.x - d.dx}px, ${d.y - d.dy}px)`;
  }

  // Scroll when held near the window's top or bottom edge.
  function tick(d) {
    if (active !== d || !d.on) return;
    const speed = (gap) => Math.ceil(((EDGE - gap) / EDGE) * 18);
    const scroller = d.item.closest("dialog") ?? null;
    const top = scroller ? scroller.getBoundingClientRect().top : 0;
    const bottom = scroller ? scroller.getBoundingClientRect().bottom : window.innerHeight;
    let by = 0;
    if (d.y < top + EDGE) by = -speed(d.y - top);
    else if (d.y > bottom - EDGE) by = speed(bottom - d.y);
    if (by) {
      if (scroller) scroller.scrollTop += by; else window.scrollBy(0, by);
      reorder(d);
    }
    requestAnimationFrame(() => tick(d));
  }

  const containers = () => [...(root.matches(opts.containers) ? [root] : []), ...root.querySelectorAll(opts.containers)];

  function containerAt(d) {
    const under = document.elementFromPoint(d.x, d.y);
    const c = under?.closest(opts.containers) ?? opts.near?.(under);
    if (c && root.contains(c)) return c;
    // Off to the side of a column: the one at that height.
    if (axis === "y") {
      return containers().find((x) => {
        const r = x.getBoundingClientRect();
        return d.y >= r.top && d.y <= r.bottom;
      }) ?? null;
    }
    return null;
  }

  // The next item after `el` in its container, skipping anything else.
  const nextItem = (el) => {
    let n = el.nextElementSibling;
    while (n && !n.matches(opts.item)) n = n.nextElementSibling;
    return n;
  };

  function reorder(d) {
    const c = containerAt(d);
    if (!c) return;
    const others = itemsOf(c, d.item);
    const before = others.find((el) => {
      const r = laidOut(el);
      if (axis === "y") return d.y < r.top + r.height / 2;
      // A grid, read row by row: before an item if above its row, or in
      // its row and left of its middle.
      return d.y < r.top || (d.y <= r.bottom && d.x < r.left + r.width / 2);
    }) ?? null;
    // Already there.
    if (d.item.parentElement === c && (before ? nextItem(d.item) === before : itemsOf(c).at(-1) === d.item)) return;
    const from = d.item.parentElement;
    const moving = [...new Set([...itemsOf(from, d.item), ...others])];
    const was = new Map(moving.map((el) => [el, laidOut(el)]));
    if (before) c.insertBefore(d.item, before);
    else if (others.length) others.at(-1).after(d.item);
    else c.prepend(d.item);
    markEmpty();
    slide(moving, was);
  }

  // Containers with no items get `.empty` (to show "Drop here").
  function markEmpty() {
    for (const c of containers()) c.classList.toggle("empty", !itemsOf(c).length);
  }

  function end(drop) {
    const d = active;
    if (!d) return;
    active = null;
    clearTimeout(d.timer);
    cancelAnimationFrame(d.frame);
    d.cleanup?.();
    if (!d.on) return;
    d.ghost.remove();
    d.item.classList.remove("drag-placeholder");
    document.body.classList.remove("is-dragging");
    const c = d.item.parentElement;
    if (!drop || !c?.matches(opts.containers)) {
      // Back where it was.
      if (d.home.parent?.isConnected) d.home.parent.insertBefore(d.item, d.home.next?.parentElement === d.home.parent ? d.home.next : null);
      markEmpty();
      opts.onCancel?.(d.item);
      return;
    }
    opts.onDrop(d.item, c, itemsOf(c).indexOf(d.item));
  }
}
