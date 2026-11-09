// How this browser shows the page: the theme (System, Light or Dark), the
// accent color, the density and the text size. Applied before the page
// draws so it doesn't flash. A classic script, loaded first in <head>; the
// header's display menu (js/themes.js) changes them through `mcastDisplay`.
//
// Each is kept in this browser (localStorage "mcast.<name>"), and a URL can
// set one for a screen without storing it: ?theme=dark&accent=teal&
// density=compact&text=large.
(function () {
  // The first of each is the default: nothing stored, no attribute set.
  var OPTIONS = {
    theme: ["system", "light", "dark"],
    accent: ["blue", "teal", "green", "purple", "orange", "pink"],
    density: ["comfortable", "compact"],
    text: ["medium", "small", "large"],
  };
  var root = document.documentElement;
  var fromUrl = {};
  try {
    var q = new URLSearchParams(location.search);
    Object.keys(OPTIONS).forEach(function (k) {
      if (OPTIONS[k].indexOf(q.get(k)) >= 0) fromUrl[k] = q.get(k);
    });
  } catch (e) {}

  function get(k) {
    if (fromUrl[k]) return fromUrl[k];
    var v = null;
    try { v = localStorage.getItem("mcast." + k); } catch (e) {}
    return OPTIONS[k].indexOf(v) >= 0 ? v : OPTIONS[k][0];
  }

  function apply(k) {
    var v = get(k);
    if (v === OPTIONS[k][0]) delete root.dataset[k];
    else root.dataset[k] = v;
    if (k === "theme") {
      var m = document.querySelector('meta[name="color-scheme"]');
      if (m) m.content = v === "system" ? "light dark" : v;
    }
  }

  // Choosing one on a screen whose URL set it changes that screen until it
  // reloads, and this browser's own choice from then on.
  function set(k, v) {
    if (!OPTIONS[k] || OPTIONS[k].indexOf(v) < 0) return;
    delete fromUrl[k];
    try {
      if (v === OPTIONS[k][0]) localStorage.removeItem("mcast." + k);
      else localStorage.setItem("mcast." + k, v);
    } catch (e) {}
    apply(k);
  }

  Object.keys(OPTIONS).forEach(apply);
  window.mcastDisplay = { OPTIONS: OPTIONS, get: get, set: set };
})();
